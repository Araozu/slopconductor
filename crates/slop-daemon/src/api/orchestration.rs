use super::chat::{
    self, AppState, PageQuery, accepted, checked_limit, storage_response, store_error,
};
use crate::{
    auth::LocalToken,
    storage::{
        StoreError,
        orchestration::{combinations, validate_spec},
    },
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use slop_protocol::{
    chat::{CancelTurnRequest, TurnControlRequest},
    orchestration::*,
};
use std::sync::{Arc, atomic::Ordering};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/tasks", get(tasks).post(create_task))
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/tasks/{id}/runs", get(runs))
        .route("/v1/tasks/{id}/events", get(events))
        .route("/v1/tasks/{id}/instructions", post(instruction))
        .route("/v1/runs/{id}", get(run))
        .route("/v1/runs/{id}/pause", post(pause))
        .route("/v1/runs/{id}/resume", post(resume))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/runs/{id}/retry", post(retry))
        .route("/v1/runs/{id}/children", get(children).post(create_child))
        .route("/v1/runs/{id}/wait", post(wait_children))
        .route("/v1/runs/{id}/result", get(run_result))
        .route("/v1/batches/preview", post(preview))
        .route("/v1/batches", get(batches).post(submit))
        .route("/v1/batches/{id}", get(batch))
        .route("/v1/batches/{id}/members", get(members))
        .route("/v1/batches/{id}/results", get(results))
        .route("/v1/batches/{id}/retry", post(retry_batch))
        .route("/v1/batches/{id}/cancel", post(cancel_batch))
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024))
        .route_layer(middleware::from_fn(chat::normalize_rejections))
        .route_layer(middleware::from_fn_with_state(
            token,
            chat::authorize_request,
        ))
}

fn admission(state: &AppState) -> Result<(), StoreError> {
    if !state.accepting.load(Ordering::Acquire) || !state.runtime.accepting_work() {
        Err(StoreError::Unavailable)
    } else {
        Ok(())
    }
}

pub(super) fn preflight(
    state: &AppState,
    mut spec: SessionSpec,
    default_max_tokens: Option<u32>,
) -> Result<SessionSpec, StoreError> {
    validate_spec(&spec)?;
    let model: slop_core::provider::ProviderModelRef =
        spec.model.parse().map_err(|_| StoreError::Invalid)?;
    let provider =
        slop_runtime::providers::provider(model.provider()).ok_or(StoreError::Invalid)?;
    let client = state.runtime.provider_client(model.provider().as_str());
    let subscription = client
        .as_ref()
        .is_some_and(|c| c.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription);
    if subscription && spec.settings.max_output_tokens.is_some() {
        return Err(StoreError::Unsupported(
            "ChatGPT subscription requests do not support an output-token cap.",
        ));
    }
    let caps = client.as_ref().map_or_else(
        || slop_runtime::providers::inference::capabilities(provider, model.model()),
        |c| c.capabilities(model.model()),
    );
    if spec
        .settings
        .reasoning_effort
        .as_ref()
        .is_some_and(|e| !caps.reasoning_efforts.contains(e))
    {
        return Err(StoreError::Unsupported("settings.reasoning_effort"));
    }
    if !subscription && spec.settings.max_output_tokens.is_none() {
        spec.settings.max_output_tokens = default_max_tokens;
    }
    Ok(spec)
}

#[utoipa::path(
    post,
    path = "/v1/tasks",
    tag = "orchestration",
    request_body = CreateTaskRequest,
    responses(
        (status = 202, description = "Task creation accepted", body = slop_protocol::orchestration::TaskReceipt),
        (status = 400, description = "Invalid spec", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn create_task(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateTaskRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let seen = match state.store.command_seen(&request.command_id).await {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    let effective = if seen {
        None
    } else {
        match preflight(
            &state,
            request.spec.clone(),
            Some(state.runtime.max_tokens()),
        ) {
            Ok(s) => Some(s),
            Err(e) => return store_error(e),
        }
    };
    accepted(state.store.create_task(request, effective).await)
}
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}",
    tag = "orchestration",
    params(("id" = String, Path, description = "Task id")),
    responses(
        (status = 200, description = "Task record", body = slop_protocol::chat::SessionResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Task not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn task(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.task(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/runs/{id}",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    responses(
        (status = 200, description = "Run record", body = slop_protocol::orchestration::RunResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn run(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.run(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/batches/{id}",
    tag = "orchestration",
    params(("id" = String, Path, description = "Batch id")),
    responses(
        (status = 200, description = "Batch record", body = slop_protocol::orchestration::BatchResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Batch not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn batch(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    storage_response(state.store.batch(&id).await)
}
#[utoipa::path(
    get,
    path = "/v1/tasks",
    tag = "orchestration",
    params(
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Tasks", body = slop_protocol::chat::Page<slop_protocol::chat::SessionResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn tasks(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.tasks(None, q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/batches",
    tag = "orchestration",
    params(
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Batches", body = slop_protocol::chat::Page<slop_protocol::orchestration::BatchResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn batches(
    State(state): State<Arc<AppState>>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.batches(q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/runs",
    tag = "orchestration",
    params(
        ("id" = String, Path, description = "Task id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Task run attempts", body = slop_protocol::chat::Page<slop_protocol::orchestration::RunResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn runs(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.runs(&id, q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/tasks/{id}/events",
    tag = "orchestration",
    params(
        ("id" = String, Path, description = "Task id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Task events", body = slop_protocol::chat::Page<slop_protocol::orchestration::TaskEventResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.task_events(&id, q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/batches/{id}/members",
    tag = "orchestration",
    params(
        ("id" = String, Path, description = "Batch id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Batch member tasks", body = slop_protocol::chat::Page<slop_protocol::chat::SessionResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn members(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.tasks(Some(&id), q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    get,
    path = "/v1/batches/{id}/results",
    tag = "orchestration",
    params(
        ("id" = String, Path, description = "Batch id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Batch member results", body = slop_protocol::chat::Page<slop_protocol::orchestration::BatchResultResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn results(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.batch_results(&id, q.after, l).await),
        Err(e) => e.into_response(),
    }
}

async fn control(
    state: Arc<AppState>,
    id: String,
    request: TurnControlRequest,
    action: &str,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let run = match state.store.run(&id).await {
        Ok(r) => r,
        Err(e) => return store_error(e),
    };
    accepted(match action {
        "pause" => state.store.pause_turn(&run.turn.id, request).await,
        "resume" => state.store.resume_turn(&run.turn.id, request).await,
        _ => {
            state
                .store
                .cancel_turn(
                    &run.turn.id,
                    CancelTurnRequest {
                        command_id: request.command_id,
                    },
                )
                .await
        }
    })
}
#[utoipa::path(
    post,
    path = "/v1/runs/{id}/pause",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    request_body = TurnControlRequest,
    responses(
        (status = 202, description = "Run pause accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn pause(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<TurnControlRequest>,
) -> Response {
    control(s, id, r, "pause").await
}
#[utoipa::path(
    post,
    path = "/v1/runs/{id}/resume",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    request_body = TurnControlRequest,
    responses(
        (status = 202, description = "Run resume accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn resume(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<TurnControlRequest>,
) -> Response {
    control(s, id, r, "resume").await
}
#[utoipa::path(
    post,
    path = "/v1/runs/{id}/cancel",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    request_body = TurnControlRequest,
    responses(
        (status = 202, description = "Run cancellation accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn cancel(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(r): Json<TurnControlRequest>,
) -> Response {
    control(s, id, r, "cancel").await
}
#[utoipa::path(
    post,
    path = "/v1/runs/{id}/retry",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    request_body = RetryRunRequest,
    responses(
        (status = 202, description = "Run retry accepted", body = slop_protocol::orchestration::TaskReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn retry(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<RetryRunRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let seen = match state.store.command_seen(&request.command_id).await {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    let effective = if seen {
        None
    } else {
        let run = match state.store.run(&id).await {
            Ok(r) => r,
            Err(e) => return store_error(e),
        };
        let task = match state.store.task(&run.task_id).await {
            Ok(t) => t,
            Err(e) => return store_error(e),
        };
        match preflight(&state, task.spec, None) {
            Ok(s) => Some(s),
            Err(e) => return store_error(e),
        }
    };
    accepted(state.store.retry_run(&id, request, effective).await)
}

async fn prepare(
    state: &AppState,
    mut spec: BatchSpec,
) -> Result<BatchPreviewResponse, StoreError> {
    spec.default_max_output_tokens = Some(
        spec.default_max_output_tokens
            .unwrap_or(state.runtime.max_tokens()),
    );
    let mut cells = combinations(&spec)?;
    for cell in &mut cells {
        cell.spec = preflight(state, cell.spec.clone(), spec.default_max_output_tokens)?;
    }
    state.store.preview_batch(spec, cells).await
}
#[utoipa::path(
    post,
    path = "/v1/batches/preview",
    tag = "orchestration",
    request_body = BatchSpec,
    responses(
        (status = 200, description = "Batch matrix preview", body = slop_protocol::orchestration::BatchPreviewResponse),
        (status = 400, description = "Invalid spec", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn preview(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<BatchSpec>,
) -> Response {
    storage_response(prepare(&state, spec).await)
}
#[utoipa::path(
    post,
    path = "/v1/batches",
    tag = "orchestration",
    request_body = CreateBatchRequest,
    responses(
        (status = 202, description = "Batch submission accepted", body = slop_protocol::orchestration::BatchReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn submit(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreateBatchRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let seen = match state.store.command_seen(&request.command_id).await {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    let prepared = if seen {
        None
    } else {
        match prepare(&state, request.spec.clone()).await {
            Ok(p) => Some(p),
            Err(e) => return store_error(e),
        }
    };
    accepted(state.store.create_batch(request, prepared).await)
}
#[utoipa::path(
    post,
    path = "/v1/batches/{id}/retry",
    tag = "orchestration",
    params(("id" = String, Path, description = "Batch id")),
    request_body = RetryBatchRequest,
    responses(
        (status = 202, description = "Batch retry accepted", body = slop_protocol::orchestration::BatchReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Batch not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn retry_batch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<RetryBatchRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let seen = match state.store.command_seen(&request.command_id).await {
        Ok(v) => v,
        Err(e) => return store_error(e),
    };
    let mut specs = Vec::new();
    if !seen {
        if request.indices.is_empty()
            || request.indices.len() > slop_core::orchestration::MAX_MATRIX_MEMBERS
        {
            return store_error(StoreError::Invalid);
        }
        for index in &request.indices {
            let task = match state.store.batch_task(&id, *index).await {
                Ok(t) => t,
                Err(e) => return store_error(e),
            };
            match preflight(&state, task.spec, None) {
                Ok(s) => specs.push(s),
                Err(e) => return store_error(e),
            }
        }
    }
    accepted(
        state
            .store
            .retry_batch(&id, request, (!seen).then_some(specs))
            .await,
    )
}

#[utoipa::path(
    post,
    path = "/v1/tasks/{id}/instructions",
    tag = "orchestration",
    params(("id" = String, Path, description = "Task id")),
    request_body = slop_protocol::chat::SendMessageRequest,
    responses(
        (status = 202, description = "Instruction accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Task not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn instruction(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<slop_protocol::chat::SendMessageRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    chat::send_message(State(state), Path(id), Json(request)).await
}

#[utoipa::path(
    post,
    path = "/v1/batches/{id}/cancel",
    tag = "orchestration",
    params(("id" = String, Path, description = "Batch id")),
    request_body = TurnControlRequest,
    responses(
        (status = 202, description = "Batch cancellation accepted", body = slop_protocol::orchestration::BatchCancelReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Batch not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn cancel_batch(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<TurnControlRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    accepted(state.store.cancel_batch(&id, request).await)
}

#[utoipa::path(
    post,
    path = "/v1/runs/{id}/children",
    tag = "orchestration",
    params(("id" = String, Path, description = "Parent run id")),
    request_body = CreateChildRequest,
    responses(
        (status = 202, description = "Child task creation accepted", body = slop_protocol::orchestration::TaskReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn create_child(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<CreateChildRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    let seen = match state.store.command_seen(&request.command_id).await {
        Ok(s) => s,
        Err(e) => return store_error(e),
    };
    let effective = if seen {
        None
    } else {
        let parent_run = match state.store.run(&id).await {
            Ok(run) => run,
            Err(e) => return store_error(e),
        };
        let parent = match state.store.task(&parent_run.task_id).await {
            Ok(task) => task,
            Err(e) => return store_error(e),
        };
        let mut spec = request.spec.clone();
        if spec.budget.is_none() {
            spec.budget = parent.spec.budget.clone();
        }
        let default_tokens = if spec.model.starts_with("codex/") {
            None
        } else {
            parent_run.turn.settings.max_output_tokens
        };
        match preflight(&state, spec, default_tokens) {
            Ok(s) => Some(s),
            Err(e) => return store_error(e),
        }
    };
    accepted(state.store.create_child(&id, request, effective).await)
}
#[utoipa::path(
    get,
    path = "/v1/runs/{id}/children",
    tag = "orchestration",
    params(
        ("id" = String, Path, description = "Parent run id"),
        ("after" = Option<u64>, Query, description = "Cursor after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Child tasks", body = slop_protocol::chat::Page<slop_protocol::chat::SessionResponse>),
        (status = 400, description = "Invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn children(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<PageQuery>,
) -> Response {
    match checked_limit(q.limit) {
        Ok(l) => storage_response(state.store.children(&id, q.after, l).await),
        Err(e) => e.into_response(),
    }
}
#[utoipa::path(
    post,
    path = "/v1/runs/{id}/wait",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    request_body = WaitChildrenRequest,
    responses(
        (status = 202, description = "Child wait accepted", body = slop_protocol::chat::CommandReceipt),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Admission closed", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn wait_children(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<WaitChildrenRequest>,
) -> Response {
    if let Err(e) = admission(&state) {
        return store_error(e);
    }
    accepted(state.store.wait_children(&id, request).await)
}
#[utoipa::path(
    get,
    path = "/v1/runs/{id}/result",
    tag = "orchestration",
    params(("id" = String, Path, description = "Run id")),
    responses(
        (status = 200, description = "Public run result", body = slop_protocol::orchestration::RunResultResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Run or result not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn run_result(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    storage_response(state.store.run_result(&id).await)
}
