use super::*;

#[utoipa::path(
    get,
    path = "/v1/capabilities",
    tag = "chat",
    responses(
        (status = 200, description = "Runtime capability advertisement", body = hive_protocol::execution::CapabilitiesResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn capabilities() -> Json<hive_protocol::execution::CapabilitiesResponse> {
    Json(hive_protocol::execution::CapabilitiesResponse {
        structured_messages: true,
        per_turn_model_selection: true,
        per_turn_settings: true,
        tools: hive_runtime::tools::definitions()
            .into_iter()
            .map(|d| hive_protocol::execution::ToolDescriptor {
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
        (status = 200, description = "Steering instructions", body = hive_protocol::chat::Page<hive_protocol::chat::SteeringInstructionResponse>),
        (status = 400, description = "Invalid limit", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Session not found", body = hive_protocol::ErrorResponse)
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
        (status = 200, description = "Per-turn model requests", body = hive_protocol::chat::Page<hive_protocol::execution::ModelRequestResponse>),
        (status = 400, description = "Invalid limit", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = hive_protocol::ErrorResponse)
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
        (status = 200, description = "Per-turn tool invocations", body = hive_protocol::chat::Page<hive_protocol::execution::ToolInvocationResponse>),
        (status = 400, description = "Invalid limit", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Turn not found", body = hive_protocol::ErrorResponse)
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
        (status = 200, description = "Tool invocation record", body = hive_protocol::execution::ToolInvocationResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Tool invocation not found", body = hive_protocol::ErrorResponse)
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
        (status = 200, description = "Message record", body = hive_protocol::chat::MessageResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Message not found", body = hive_protocol::ErrorResponse)
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
        (status = 200, description = "Artifact metadata", body = hive_protocol::execution::ArtifactResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Artifact not found", body = hive_protocol::ErrorResponse)
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
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 404, description = "Artifact unavailable", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Artifact data inconsistent", body = hive_protocol::ErrorResponse)
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
    post,
    path = "/v1/artifacts",
    tag = "chat",
    request_body(
        content = Vec<u8>,
        description = "Raw image bytes. Content-Type must be image/png, image/jpeg, image/gif, or image/webp. Max 20 MiB."
    ),
    responses(
        (status = 201, description = "Image artifact stored (content-addressed, idempotent)", body = hive_protocol::execution::ArtifactResponse),
        (status = 400, description = "Empty or unrecognized image bytes", body = hive_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse),
        (status = 413, description = "Image exceeds the 20 MiB limit", body = hive_protocol::ErrorResponse),
        (status = 415, description = "Unsupported media type", body = hive_protocol::ErrorResponse),
        (status = 503, description = "Artifact storage unavailable", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn upload_artifact(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    use hive_runtime::providers::inference;
    let declared = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.split(';').next().unwrap_or("").trim().to_lowercase())
        .unwrap_or_default();
    if !inference::ALLOWED_IMAGE_MEDIA_TYPES.contains(&declared.as_str()) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Content-Type must be image/png, image/jpeg, image/gif, or image/webp.",
        );
    }
    let bytes = match axum::body::to_bytes(body, inference::MAX_IMAGE_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                "The image exceeds the 20 MiB limit.",
            );
        }
    };
    if bytes.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_image",
            "The image bytes are empty.",
        );
    }
    let artifact = match state
        .runtime
        .tools()
        .save_image_bytes(&bytes, &declared)
        .await
    {
        Ok(artifact) => artifact,
        Err("unsupported_media_type") => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "Content-Type must be image/png, image/jpeg, image/gif, or image/webp.",
            );
        }
        Err("invalid_image") => {
            return error(
                StatusCode::BAD_REQUEST,
                "invalid_image",
                "The image bytes are empty or do not match the declared Content-Type.",
            );
        }
        Err("image_too_large") => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "image_too_large",
                "The image exceeds the 20 MiB limit.",
            );
        }
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "artifact_unavailable",
                "Artifact storage is unavailable.",
            );
        }
    };
    match state
        .store
        .register_artifact(&artifact.id, artifact.size_bytes, &artifact.media_type)
        .await
    {
        Ok(metadata) => (StatusCode::CREATED, Json(metadata)).into_response(),
        Err(error) => store_error(error),
    }
}

#[utoipa::path(
    get,
    path = "/v1/models",
    tag = "chat",
    responses(
        (status = 200, description = "Executable model catalog with provider-specific reasoning efforts from refreshed public metadata and compiled fallbacks", body = Vec<ModelResponse>),
        (status = 401, description = "Missing or invalid bearer token", body = hive_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn models(State(state): State<Arc<AppState>>) -> Response {
    let mut items = Vec::new();
    for id in hive_core::provider::ProviderId::all() {
        let Some(provider) = hive_runtime::providers::provider(*id) else {
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
        for model in hive_runtime::providers::catalog::models(provider) {
            let client_caps = client.as_ref().map(|c| c.capabilities(&model.id));
            let fallback_caps =
                hive_runtime::providers::inference::capabilities(provider, &model.id);
            let effective = client_caps.as_ref().unwrap_or(&fallback_caps);
            let subscription = client.as_ref().is_some_and(|c| {
                c.auth_mode() == hive_runtime::providers::AuthMode::ChatGptSubscription
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
                capabilities: hive_protocol::execution::ModelCapabilities {
                    tools: effective.tools,
                    context_window_tokens: fallback_caps.context_window_tokens,
                    max_input_tokens: fallback_caps.max_input_tokens,
                    reasoning_efforts: effective.reasoning_efforts.clone(),
                    incremental_streaming: true,
                    max_output_tokens: if cap_supported { 65_536 } else { 0 },
                    output_token_cap_supported: cap_supported,
                    image_input: effective.image_input,
                },
            });
        }
    }
    Json(items).into_response()
}
