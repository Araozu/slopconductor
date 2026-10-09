use std::{collections::VecDeque, convert::Infallible, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{Request, StatusCode, header::CONTENT_TYPE},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::stream;
use serde::Deserialize;
use slop_protocol::{
    ErrorResponse,
    chat::{
        CancelTurnRequest, CreateSessionRequest, EventFrame, EventResponse, ModelResponse,
        SendMessageRequest,
    },
};

use crate::{
    auth::LocalToken,
    storage::{StoreClient, StoreError},
};

#[derive(Clone)]
pub struct AppState {
    pub store: StoreClient,
    pub token: Arc<LocalToken>,
    pub runtime: slop_runtime::chat::ChatRuntime,
    pub accepting: Arc<std::sync::atomic::AtomicBool>,
    pub event_subscribers: Arc<tokio::sync::Semaphore>,
    pub credentials: Arc<crate::credentials::ProviderCredentials>,
}

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/models", get(models))
        .route("/v1/sessions", get(sessions).post(create_session))
        .route("/v1/sessions/{session_id}", get(session))
        .route(
            "/v1/sessions/{session_id}/messages",
            get(messages).post(send_message),
        )
        .route("/v1/sessions/{session_id}/events", get(events))
        .route("/v1/turns/{turn_id}", get(turn))
        .route("/v1/turns/{turn_id}/requests", get(model_requests))
        .route("/v1/turns/{turn_id}/tools", get(tool_invocations))
        .route("/v1/tools/{id}", get(tool_invocation))
        .route("/v1/messages/{id}", get(message))
        .route("/v1/artifacts/{id}", get(artifact))
        .route("/v1/artifacts/{id}/content", get(artifact_content))
        .route("/v1/turns/{turn_id}/cancel", post(cancel_turn))
        .route_layer(middleware::from_fn(normalize_rejections))
        .route_layer(middleware::from_fn_with_state(token, authorize_request))
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EventQuery {
    after: Option<u64>,
    follow: Option<bool>,
}

async fn capabilities() -> Json<slop_protocol::execution::CapabilitiesResponse> {
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
        max_model_requests_per_turn: 64,
        max_tool_calls_per_turn: 128,
    })
}
async fn model_requests(
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
async fn tool_invocations(
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
async fn tool_invocation(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.tool_invocation(&id).await)
}
async fn message(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.message(&id).await)
}
async fn artifact(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.artifact(&id).await)
}
async fn artifact_content(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
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

async fn models(State(state): State<Arc<AppState>>) -> Response {
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
            let tools = *id != slop_core::provider::ProviderId::Codex;
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
                    reasoning_efforts: client_caps
                        .map_or_else(Vec::new, |caps| caps.reasoning_efforts),
                    incremental_streaming: true,
                    max_output_tokens: if cap_supported { 65_536 } else { 0 },
                    output_token_cap_supported: cap_supported,
                },
            });
        }
    }
    Json(items).into_response()
}

async fn sessions(State(state): State<Arc<AppState>>, Query(query): Query<PageQuery>) -> Response {
    let limit = match checked_limit(query.limit) {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    storage_response(state.store.sessions(query.after, limit).await)
}

async fn create_session(
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
        if provider_id == slop_core::provider::ProviderId::Codex
            && request
                .execution
                .as_ref()
                .is_some_and(|policy| !policy.allowed_tools.is_empty())
        {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_capability",
                "Codex text requests do not support workspace tools.",
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

async fn session(State(state): State<Arc<AppState>>, Path(session_id): Path<String>) -> Response {
    storage_response(state.store.session(&session_id).await)
}

async fn messages(
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

async fn send_message(
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
    let full_model = request
        .model
        .clone()
        .unwrap_or_else(|| format!("{}/{}", session.provider, session.model));
    let Ok(model_ref) = full_model.parse::<slop_core::provider::ProviderModelRef>() else {
        return error(
            StatusCode::BAD_REQUEST,
            "unsupported_model",
            "The requested model is not supported.",
        );
    };
    let Some(provider) = slop_runtime::providers::provider(model_ref.provider()) else {
        return error(
            StatusCode::BAD_REQUEST,
            "unsupported_model",
            "The requested model is not supported.",
        );
    };
    if provider.wire_protocol(model_ref.model()).is_err() {
        return error(
            StatusCode::BAD_REQUEST,
            "unsupported_model",
            "The requested model is not supported.",
        );
    }
    // Keep one auth/capability snapshot through preflight and settings freeze.
    let client = state.runtime.provider_client(model_ref.provider().as_str());
    let subscription = client.as_ref().is_some_and(|client| {
        client.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription
    });
    {
        let cap = request
            .settings
            .as_ref()
            .and_then(|settings| settings.max_output_tokens)
            .or(session.settings.max_output_tokens);
        if model_ref.provider() == slop_core::provider::ProviderId::Codex
            && subscription
            && cap.is_some()
        {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_capability",
                "ChatGPT subscription requests do not support an output-token cap.",
            );
        }
        if model_ref.provider() == slop_core::provider::ProviderId::Codex
            && session
                .execution
                .as_ref()
                .is_some_and(|policy| !policy.allowed_tools.is_empty())
        {
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_capability",
                "Codex text requests do not support workspace tools.",
            );
        }
        let (has_non_text, required) = match state.store.context_requirements(&session_id).await {
            Ok(value) => value,
            Err(error) => return store_error(error),
        };
        let requested_wire = provider
            .wire_protocol(model_ref.model())
            .ok()
            .map(|value| value.as_str().to_owned());
        if context_incompatibility(
            model_ref.provider().as_str(),
            model_ref.model(),
            requested_wire.as_deref().unwrap_or_default(),
            has_non_text,
            required,
        )
        .is_some()
        {
            return error(
                StatusCode::BAD_REQUEST,
                "context_incompatible",
                "The selected model cannot replay the stored structured context or required provider continuation.",
            );
        }
    }
    let mut effective = request
        .settings
        .clone()
        .unwrap_or_else(|| session.settings.clone());
    effective.max_output_tokens = effective
        .max_output_tokens
        .or(session.settings.max_output_tokens);
    if effective.max_output_tokens.is_none() && !subscription {
        effective.max_output_tokens = Some(state.runtime.max_tokens());
    }
    {
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
            return error(
                StatusCode::BAD_REQUEST,
                "unsupported_capability",
                "The selected model does not support the requested reasoning setting.",
            );
        }
    }
    accepted(
        state
            .store
            .send_message_with_effective_settings(&session_id, request, Some(effective))
            .await,
    )
}

fn context_incompatibility(
    provider: &str,
    model: &str,
    wire: &str,
    has_non_text: bool,
    required: Vec<(String, String, String)>,
) -> Option<&'static str> {
    if provider == "codex" && has_non_text {
        return Some("context_incompatible");
    }
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

async fn turn(State(state): State<Arc<AppState>>, Path(turn_id): Path<String>) -> Response {
    storage_response(state.store.turn(&turn_id).await)
}

async fn cancel_turn(
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

async fn events(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<EventQuery>,
) -> Response {
    let after = query.after.unwrap_or(0);
    let session = match state.store.session(&session_id).await {
        Ok(session) => session,
        Err(error) => return store_error(error),
    };
    if after > session.last_event_sequence {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The event cursor is ahead of the durable session history.",
        );
    }
    let subscriber = match state.event_subscribers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "subscriber_limit",
                "The event subscriber limit has been reached.",
            );
        }
    };
    // Subscribe before the first durable catch-up read so a concurrent commit
    // is observed either in that read or by the wake-up which follows it.
    let receiver = state.runtime.subscribe();
    let feed = EventFeed {
        state,
        session_id,
        cursor: after,
        follow: query.follow.unwrap_or(false),
        receiver,
        pending: VecDeque::new(),
        heartbeat_at: tokio::time::Instant::now() + Duration::from_secs(5),
        _subscriber: subscriber,
    };
    let body = Body::from_stream(stream::unfold(feed, |mut feed| async move {
        let frame = feed.next_frame().await?;
        let bytes = serde_json::to_vec(&frame).ok()?;
        Some((
            Ok::<Bytes, Infallible>(Bytes::from([bytes, vec![b'\n']].concat())),
            feed,
        ))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

struct EventFeed {
    state: Arc<AppState>,
    session_id: String,
    cursor: u64,
    follow: bool,
    receiver: tokio::sync::broadcast::Receiver<slop_runtime::chat::ChatDelta>,
    pending: VecDeque<EventResponse>,
    heartbeat_at: tokio::time::Instant,
    _subscriber: tokio::sync::OwnedSemaphorePermit,
}

impl EventFeed {
    async fn next_frame(&mut self) -> Option<EventFrame> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.cursor = event.sequence;
                return Some(EventFrame::Durable { event });
            }
            match self
                .state
                .store
                .events(&self.session_id, self.cursor, 100)
                .await
            {
                Ok(page) if !page.items.is_empty() => {
                    self.pending.extend(page.items);
                    continue;
                }
                Ok(_) if !self.follow => return None,
                Err(_) => return None,
                Ok(_) => {}
            }
            if !self.follow
                || !self
                    .state
                    .accepting
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return None;
            }
            tokio::select! {
                delta = self.receiver.recv() => match delta {
                    Ok(delta) if delta.session_id == self.session_id => return Some(EventFrame::Delta {
                        session_id: delta.session_id,
                        turn_id: delta.turn_id,
                        text: delta.text,
                        message_id: delta.message_id,
                        block_id: delta.block_id,
                        request_id: delta.request_id,
                        invocation_id: delta.invocation_id,
                        stream_id: delta.stream_id,
                        chunk_index: delta.chunk_index,
                        kind: delta.kind,
                    }),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) | Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                },
                _ = tokio::time::sleep_until(self.heartbeat_at) => {
                    self.heartbeat_at = tokio::time::Instant::now() + Duration::from_secs(5);
                    return Some(EventFrame::Heartbeat);
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }
}

pub(super) async fn authorize_request(
    State(token): State<Arc<LocalToken>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let values = request.headers().get_all(axum::http::header::AUTHORIZATION);
    let mut values = values.iter();
    let valid = values
        .next()
        .and_then(|value| {
            if values.next().is_some() {
                return None;
            }
            let value = value.to_str().ok()?;
            let candidate = crate::auth::bearer_token(value)?;
            Some(token.matches(candidate))
        })
        .unwrap_or(false);
    if valid {
        next.run(request).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

pub(super) async fn normalize_rejections(request: Request<Body>, next: Next) -> Response {
    let response = next.run(request).await;
    // Preserve our typed errors; only normalize plain extractor rejections.
    if response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        == Some("application/json")
    {
        return response;
    }
    match response.status() {
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request could not be parsed or failed validation.",
        ),
        StatusCode::PAYLOAD_TOO_LARGE => error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "The request body exceeds the supported limit.",
        ),
        _ => response,
    }
}

fn checked_limit(limit: Option<usize>) -> Result<usize, ApiError> {
    let limit = limit.unwrap_or(50);
    if (1..=200).contains(&limit) {
        Ok(limit)
    } else {
        Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_limit",
            message: "The page limit must be between 1 and 200.",
        })
    }
}

fn accepted<T: serde::Serialize>(result: Result<T, StoreError>) -> Response {
    match result {
        Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
        Err(error) => store_error(error),
    }
}

fn storage_response<T: serde::Serialize>(result: Result<T, StoreError>) -> Response {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => store_error(error),
    }
}

fn store_error(store_error: StoreError) -> Response {
    match store_error {
        StoreError::Busy => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_busy",
            "Local storage is busy; retry using the same command id.",
        ),
        StoreError::Invalid => error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The chat request is invalid.",
        ),
        StoreError::Unsupported(field) => {
            error(StatusCode::BAD_REQUEST, "unsupported_capability", field)
        }
        StoreError::Limit => error(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_limit",
            "The configured queue or context limit has been reached.",
        ),
        StoreError::NotFound => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested chat record does not exist.",
        ),
        StoreError::Conflict => error(
            StatusCode::CONFLICT,
            "command_conflict",
            "The command conflicts with the current durable state.",
        ),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "Local storage is temporarily unavailable.",
        ),
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    )
        .into_response()
}

struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: "A valid bearer token is required.",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error(self.status, self.code, self.message)
    }
}

#[cfg(test)]
mod provider_preflight_tests {
    use super::context_incompatibility;

    #[test]
    fn rejects_known_incompatible_continuation_but_allows_fresh_go_to_zen_context() {
        assert!(
            context_incompatibility(
                "codex",
                "gpt-6.1-sol",
                "responses",
                false,
                vec![(
                    "opencode-go".into(),
                    "gpt-6.1-sol".into(),
                    "responses".into()
                )],
            )
            .is_some()
        );
        assert!(
            context_incompatibility("codex", "gpt-6.1-sol", "responses", true, vec![]).is_some()
        );
        assert!(
            context_incompatibility("opencode-zen", "glm-5.3", "chat_completions", true, vec![])
                .is_none()
        );
        assert!(
            context_incompatibility(
                "opencode-zen",
                "glm-5.3",
                "chat_completions",
                false,
                vec![
                    (
                        "opencode-zen".into(),
                        "glm-5.3".into(),
                        "chat_completions".into()
                    ),
                    (
                        "opencode-go".into(),
                        "glm-5.3-flash".into(),
                        "chat_completions".into()
                    ),
                ],
            )
            .is_some()
        );
    }
}
