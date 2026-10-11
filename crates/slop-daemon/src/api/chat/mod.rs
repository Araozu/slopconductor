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
        .route(
            "/v1/sessions/{session_id}/instructions",
            get(steering_instructions),
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
        .route("/v1/turns/{turn_id}/pause", post(pause_turn))
        .route("/v1/turns/{turn_id}/resume", post(resume_turn))
        .route_layer(middleware::from_fn(normalize_rejections))
        .route_layer(middleware::from_fn_with_state(token, authorize_request))
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub(super) after: Option<u64>,
    pub(super) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct EventQuery {
    after: Option<u64>,
    follow: Option<bool>,
}

mod events;
mod execution;
#[cfg(test)]
mod provider_preflight_tests;
mod sessions;
mod support;
mod turns;

use events::*;
use execution::*;
use sessions::*;
pub(in crate::api) use support::error;
pub(in crate::api) use support::{
    accepted, authorize_request, checked_limit, normalize_rejections, storage_response, store_error,
};
use turns::*;
