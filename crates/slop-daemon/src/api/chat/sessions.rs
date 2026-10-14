use super::*;

#[utoipa::path(
    post,
    path = "/v1/sessions/{session_id}/inbox",
    tag = "chat",
    params(("session_id" = String, Path, description = "Session id")),
    request_body = slop_protocol::chat::UpdateSessionInboxRequest,
    responses(
        (status = 202, description = "Inbox update accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn update_session_inbox(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<slop_protocol::chat::UpdateSessionInboxRequest>,
) -> Response {
    accepted(state.store.update_session_inbox(&id, request).await)
}

#[utoipa::path(
    get,
    path = "/v1/drafts",
    tag = "chat",
    params(
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Saved compose drafts", body = slop_protocol::chat::Page<slop_protocol::chat::ConversationDraft>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn drafts(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.drafts(query.after, limit).await)
}

#[utoipa::path(
    put,
    path = "/v1/drafts/{id}",
    tag = "chat",
    params(("id" = String, Path, description = "Draft id")),
    request_body = slop_protocol::chat::SaveConversationDraftRequest,
    responses(
        (status = 200, description = "Saved draft", body = slop_protocol::chat::ConversationDraft),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn save_draft(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<slop_protocol::chat::SaveConversationDraftRequest>,
) -> Response {
    storage_response(state.store.save_draft(&id, request).await)
}

#[utoipa::path(
    delete,
    path = "/v1/drafts/{id}",
    tag = "chat",
    params(("id" = String, Path, description = "Draft id")),
    responses(
        (status = 200, description = "Draft removed"),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Draft not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remove_draft(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.remove_draft(&id).await)
}

#[utoipa::path(
    get,
    path = "/v1/sessions",
    tag = "chat",
    params(
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Durable sessions", body = slop_protocol::chat::Page<slop_protocol::chat::SessionResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn sessions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.sessions(query.after, limit).await)
}

#[utoipa::path(
    post,
    path = "/v1/sessions",
    tag = "chat",
    request_body = CreateSessionRequest,
    responses(
        (status = 202, description = "Session creation accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request or unsupported model", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down or runtime unavailable", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateSessionRequest>,
) -> Response {
    if !state.accepting.load(std::sync::atomic::Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    if !state.runtime.accepting_work() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "The chat execution runtime is unavailable.",
        );
    }
    match state.store.command_seen(&request.command_id).await {
        Ok(true) => return accepted(state.store.create_session(request).await),
        Err(error) => return store_error(error),
        Ok(false) => {}
    }
    let selected_client = state.runtime.provider_client(&request.provider);
    let selected_subscription = selected_client.as_ref().is_some_and(|client| {
        client.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription
    });
    {
        let Some(provider_id) = request
            .provider
            .parse::<slop_core::provider::ProviderId>()
            .ok()
        else {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_model",
                "The requested model is not supported.",
            );
        };
        let Some(provider) = slop_runtime::providers::provider(provider_id) else {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_model",
                "The requested model is not supported.",
            );
        };
        if provider.wire_protocol(&request.model).is_err() {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_model",
                "The requested model is not supported.",
            );
        }
        let explicitly_capped = request.max_tokens.is_some()
            || request
                .settings
                .as_ref()
                .is_some_and(|settings| settings.max_output_tokens.is_some());
        if provider_id == slop_core::provider::ProviderId::Codex
            && selected_subscription
            && explicitly_capped
        {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_capability",
                "ChatGPT subscription requests do not support an output-token cap.",
            );
        }
    }
    if let Some(prompt) = &request.prompt {
        let spec = slop_protocol::orchestration::SessionSpec {
            title: request.title.clone(),
            prompt: prompt.clone(),
            model: format!("{}/{}", request.provider, request.model),
            settings: request.settings.clone().unwrap_or_default(),
            project: request.project.clone(),
            budget: request.budget.clone(),
            orchestration: request.orchestration.clone(),
        };
        if let Err(error) =
            crate::api::orchestration::preflight(&state, spec, Some(state.runtime.max_tokens()))
        {
            return store_error(error);
        }
    }
    accepted(
        state
            .store
            .create_session_with_default_max_tokens(
                request,
                if selected_subscription {
                    None
                } else {
                    Some(state.runtime.max_tokens())
                },
            )
            .await,
    )
}

#[utoipa::path(
    get,
    path = "/v1/sessions/{session_id}",
    tag = "chat",
    params(("session_id" = String, Path, description = "Session id")),
    responses(
        (status = 200, description = "Session record", body = slop_protocol::chat::SessionResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    storage_response(state.store.session(&session_id).await)
}

#[utoipa::path(
    get,
    path = "/v1/sessions/{session_id}/messages",
    tag = "chat",
    params(
        ("session_id" = String, Path, description = "Session id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Session messages", body = slop_protocol::chat::Page<slop_protocol::chat::MessageResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn messages(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.messages(&session_id, query.after, limit).await)
}

#[utoipa::path(
    post,
    path = "/v1/sessions/{session_id}/messages",
    tag = "chat",
    params(("session_id" = String, Path, description = "Session id")),
    request_body = SendMessageRequest,
    responses(
        (status = 202, description = "Message accepted for execution", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request or unsupported model", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down or runtime unavailable", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn send_message(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    if !state.accepting.load(std::sync::atomic::Ordering::Acquire) {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "shutting_down",
            "The daemon is shutting down.",
        );
    }
    if !state.runtime.accepting_work() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime_unavailable",
            "The chat execution runtime is unavailable.",
        );
    }
    match state.store.command_seen(&request.command_id).await {
        Ok(true) => return accepted(state.store.send_message(&session_id, request).await),
        Err(error) => return store_error(error),
        Ok(false) => {}
    }
    let session = match state.store.session(&session_id).await {
        Ok(session) => session,
        Err(error) => return store_error(error),
    };
    let fallback = preflight_turn(
        &state,
        &session,
        request.model.as_deref(),
        request.settings.as_ref(),
    );
    let (fallback_settings, fallback_error) = match fallback {
        Ok(settings) => (settings, None),
        Err((code, message)) => (session.settings.clone(), Some((code, message))),
    };
    if request.delivery.unwrap_or_default() == slop_protocol::chat::DeliveryMode::AfterTurn
        && let Some((code, message)) = fallback_error
    {
        return error(StatusCode::BAD_REQUEST, code, message);
    }
    accepted(
        state
            .store
            .send_message_preflighted(&session_id, request, fallback_settings, fallback_error)
            .await,
    )
}

pub(super) fn preflight_turn(
    state: &AppState,
    session: &slop_protocol::chat::SessionResponse,
    selected_model: Option<&str>,
    requested_settings: Option<&slop_protocol::execution::GenerationSettings>,
) -> Result<slop_protocol::execution::GenerationSettings, (&'static str, &'static str)> {
    use slop_core::provider::ProviderModelRef;
    let full_model = selected_model.map_or_else(
        || format!("{}/{}", session.provider, session.model),
        str::to_owned,
    );
    let model_ref = full_model
        .parse::<ProviderModelRef>()
        .map_err(|_| ("unsupported_model", "The requested model is not supported."))?;
    let provider = slop_runtime::providers::provider(model_ref.provider())
        .ok_or(("unsupported_model", "The requested model is not supported."))?;
    if provider.wire_protocol(model_ref.model()).is_err() {
        return Err(("unsupported_model", "The requested model is not supported."));
    }
    let client = state.runtime.provider_client(model_ref.provider().as_str());
    let subscription = client.as_ref().is_some_and(|client| {
        client.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription
    });
    let mut effective = requested_settings
        .cloned()
        .unwrap_or_else(|| session.settings.clone());
    effective.max_output_tokens = effective
        .max_output_tokens
        .or(session.settings.max_output_tokens);
    effective.full_context = effective.full_context.or(session.settings.full_context);
    if model_ref.provider() == slop_core::provider::ProviderId::Codex
        && subscription
        && effective.max_output_tokens.is_some()
    {
        if model_ref.provider().as_str() != session.provider
            && requested_settings
                .and_then(|settings| settings.max_output_tokens)
                .is_none()
        {
            // A different provider's default cap is not an explicit request
            // for an unsupported subscription setting on this new turn.
            effective.max_output_tokens = None;
        } else {
            return Err((
                "unsupported_capability",
                "ChatGPT subscription requests do not support an output-token cap.",
            ));
        }
    }
    if effective.max_output_tokens.is_none() && !subscription {
        effective.max_output_tokens = Some(state.runtime.max_tokens());
    }
    if provider.requires_output_limit() && effective.max_output_tokens.is_none() {
        effective.max_output_tokens = Some(state.runtime.max_tokens());
    }
    let caps = client.as_ref().map_or_else(
        || slop_runtime::providers::inference::capabilities(provider, model_ref.model()),
        |client| client.capabilities(model_ref.model()),
    );
    if effective.full_context == Some(true) && caps.context_window_tokens.is_none() {
        return Err((
            "unsupported_capability",
            "The model's full context window is unknown.",
        ));
    }
    if effective
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| !caps.reasoning_efforts.contains(effort))
    {
        return Err((
            "unsupported_capability",
            "The selected model does not support the requested reasoning setting.",
        ));
    }
    Ok(effective)
}
