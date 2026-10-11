use super::*;

pub(super) async fn sessions(
    State(state): State<Arc<AppState>>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.sessions(query.after, limit).await)
}

pub(super) async fn create_session(
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

pub(super) async fn session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Response {
    storage_response(state.store.session(&session_id).await)
}

pub(super) async fn messages(
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

pub(super) async fn send_message(
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
        &session_id,
        &session,
        request.model.as_deref(),
        request.settings.as_ref(),
    )
    .await;
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

pub(super) async fn preflight_turn(
    state: &AppState,
    session_id: &str,
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
    if model_ref.provider() == slop_core::provider::ProviderId::Codex
        && subscription
        && effective.max_output_tokens.is_some()
    {
        return Err((
            "unsupported_capability",
            "ChatGPT subscription requests do not support an output-token cap.",
        ));
    }
    let (has_non_text, required) =
        state
            .store
            .context_requirements(session_id)
            .await
            .map_err(|_| {
                (
                    "storage_unavailable",
                    "Stored context could not be checked.",
                )
            })?;
    let wire = provider
        .wire_protocol(model_ref.model())
        .ok()
        .map(|value| value.as_str().to_owned())
        .unwrap_or_default();
    if context_incompatibility(
        model_ref.provider().as_str(),
        model_ref.model(),
        &wire,
        has_non_text,
        required,
    )
    .is_some()
    {
        return Err((
            "context_incompatible",
            "The selected model cannot replay the stored structured context or required provider continuation.",
        ));
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

pub(super) fn context_incompatibility(
    provider: &str,
    model: &str,
    wire: &str,
    _has_non_text: bool,
    required: Vec<(String, String, String)>,
) -> Option<&'static str> {
    if required
        .into_iter()
        .any(|(saved_provider, saved_model, saved_wire)| {
            saved_provider != provider || saved_model != model || saved_wire != wire
        })
    {
        return Some("context_incompatible");
    }
    None
}
