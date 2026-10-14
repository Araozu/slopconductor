use super::*;

#[utoipa::path(
    get,
    path = "/v1/capabilities",
    tag = "chat",
    responses(
        (status = 200, description = "Runtime capability advertisement", body = slop_protocol::execution::CapabilitiesResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn capabilities() -> Json<slop_protocol::execution::CapabilitiesResponse> {
    Json(slop_protocol::execution::CapabilitiesResponse {
        structured_messages: true,
        per_turn_model_selection: true,
        per_turn_settings: true,
        tools: slop_runtime::tools::definitions()
            .into_iter()
            .map(|d| slop_protocol::execution::ToolDescriptor {
                side_effects: if d.name == "read" { "read" } else { "write" }.into(),
                name: d.name,
                description: d.description,
                parameters: d.parameters,
            })
            .collect(),
        max_model_requests_per_turn: u32::MAX,
        max_tool_calls_per_turn: u32::MAX,
        execution_steering: true,
        turn_pause_resume: true,
    })
}

#[utoipa::path(
    get,
    path = "/v1/sessions/{session_id}/instructions",
    tag = "chat",
    params(
        ("session_id" = String, Path, description = "Session id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Steering instructions", body = slop_protocol::chat::Page<slop_protocol::chat::SteeringInstructionResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn steering_instructions(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    storage_response(
        state
            .store
            .steering_instructions(&session_id, query.after, limit)
            .await,
    )
}
#[utoipa::path(
    get,
    path = "/v1/turns/{turn_id}/requests",
    tag = "chat",
    params(
        ("turn_id" = String, Path, description = "Turn id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Per-turn model requests", body = slop_protocol::chat::Page<slop_protocol::execution::ModelRequestResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn model_requests(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.model_requests(&id, query.after, limit).await)
}
#[utoipa::path(
    get,
    path = "/v1/turns/{turn_id}/tools",
    tag = "chat",
    params(
        ("turn_id" = String, Path, description = "Turn id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Per-turn tool invocations", body = slop_protocol::chat::Page<slop_protocol::execution::ToolInvocationResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn tool_invocations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.tool_invocations(&id, query.after, limit).await)
}
#[utoipa::path(
    get,
    path = "/v1/tools/{id}",
    tag = "chat",
    params(("id" = String, Path, description = "Tool invocation id")),
    responses(
        (status = 200, description = "Tool invocation record", body = slop_protocol::execution::ToolInvocationResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Tool invocation not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn tool_invocation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.tool_invocation(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/messages/{id}",
    tag = "chat",
    params(("id" = String, Path, description = "Message id")),
    responses(
        (status = 200, description = "Message record", body = slop_protocol::chat::MessageResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Message not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn message(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.message(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/artifacts/{id}",
    tag = "chat",
    params(("id" = String, Path, description = "Artifact id")),
    responses(
        (status = 200, description = "Artifact metadata", body = slop_protocol::execution::ArtifactResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Artifact not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn artifact(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.artifact(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/artifacts/{id}/content",
    tag = "chat",
    params(("id" = String, Path, description = "Artifact id")),
    responses(
        (status = 200, description = "Artifact bytes", content_type = "application/octet-stream"),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Artifact unavailable", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Artifact data inconsistent", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn artifact_content(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let metadata = match state.store.artifact(&id).await {
        Ok(metadata) => metadata,
        Err(error) => return store_error(error),
    };
    let Some(path) = state.runtime.tools().artifact_path(&id) else {
        return error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Artifact is unavailable.",
        );
    };
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(_) => {
            return error(
                StatusCode::NOT_FOUND,
                "artifact_unavailable",
                "Artifact data is unavailable.",
            );
        }
    };
    if file
        .metadata()
        .await
        .map_or(true, |m| !m.is_file() || m.len() != metadata.size_bytes)
    {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "artifact_unavailable",
            "Artifact data is inconsistent.",
        );
    }
    let body = Body::from_stream(stream::try_unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt;
        let mut bytes = vec![0u8; 16 * 1024];
        let n = file.read(&mut bytes).await?;
        if n == 0 {
            Ok::<_, std::io::Error>(None)
        } else {
            bytes.truncate(n);
            Ok(Some((Bytes::from(bytes), file)))
        }
    }));
    Response::builder()
        .header(CONTENT_TYPE, "application/octet-stream")
        .header("content-length", metadata.size_bytes)
        .header("content-disposition", "attachment")
        .header("x-content-type-options", "nosniff")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[utoipa::path(
    get,
    path = "/v1/models",
    tag = "chat",
    responses(
        (status = 200, description = "Executable model catalog", body = Vec<ModelResponse>),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn models(State(state): State<Arc<AppState>>) -> Response {
    let mut items = Vec::new();
    for id in slop_core::provider::ProviderId::all() {
        let Some(provider) = slop_runtime::providers::provider(*id) else {
            continue;
        };
        let client = state.runtime.provider_client(id.as_str());
        let ready = client.is_some() && state.runtime.accepting_work();
        let reason = if ready {
            None
        } else if !state.runtime.accepting_work() {
            Some("chat runtime is unavailable".to_owned())
        } else {
            Some(format!("{} credentials are unavailable", id.as_str()))
        };
        for model in provider.models() {
            let client_caps = client.as_ref().map(|c| c.capabilities(model.id));
            let fallback_caps =
                slop_runtime::providers::inference::capabilities(provider, model.id);
            let tools = client_caps.as_ref().unwrap_or(&fallback_caps).tools;
            let subscription = client.as_ref().is_some_and(|c| {
                c.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription
            });
            let cap_supported = !subscription;
            items.push(ModelResponse {
                id: format!("{}/{}", id.as_str(), model.id),
                provider: id.as_str().to_owned(),
                model: model.id.to_owned(),
                display_name: model.display_name.to_owned(),
                ready,
                is_default: format!("{}/{}", id.as_str(), model.id)
                    == state.runtime.default_model(),
                reason: reason.clone(),
                capabilities: slop_protocol::execution::ModelCapabilities {
                    tools,
                    context_window_tokens: fallback_caps.context_window_tokens,
                    max_input_tokens: fallback_caps.max_input_tokens,
                    reasoning_efforts: client_caps.unwrap_or(fallback_caps).reasoning_efforts,
                    incremental_streaming: true,
                    max_output_tokens: if cap_supported { 65_536 } else { 0 },
                    output_token_cap_supported: cap_supported,
                },
            });
        }
    }
    Json(items).into_response()
}
