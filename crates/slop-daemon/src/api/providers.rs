use std::sync::{Arc, atomic::Ordering};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use slop_protocol::{
    ErrorResponse, PROVIDERS_PATH,
    providers::{SetApiKeyRequest, StartLoginRequest},
};

use super::chat::{AppState, authorize_request, normalize_rejections};
use crate::{auth::LocalToken, credentials::CredentialError};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route(PROVIDERS_PATH, get(statuses))
        .route("/v1/providers/{provider}/api-key", put(set_api_key))
        .route("/v1/providers/codex/login", post(start_login))
        .route("/v1/providers/codex/login/{login_id}", get(login_status))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .route_layer(middleware::from_fn(normalize_rejections))
        .route_layer(middleware::from_fn_with_state(token, authorize_request))
}

async fn statuses(State(state): State<Arc<AppState>>) -> Response {
    Json(state.credentials.statuses().await).into_response()
}

async fn set_api_key(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
    Json(request): Json<SetApiKeyRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    respond(
        state
            .credentials
            .set_api_key(provider, request.api_key, state.runtime.clone())
            .await,
    )
}

async fn start_login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartLoginRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    match state.credentials.start_login(request.command_id).await {
        Ok(login) => (StatusCode::ACCEPTED, Json(login)).into_response(),
        Err(error) => credential_error(error),
    }
}

async fn login_status(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    respond(state.credentials.login_status(&id).await)
}

fn respond<T: serde::Serialize>(result: Result<T, CredentialError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => credential_error(error),
    }
}

fn credential_error(error: CredentialError) -> Response {
    let (status, code) = match error {
        CredentialError::Unsupported => (StatusCode::BAD_REQUEST, "unsupported_provider"),
        CredentialError::InvalidKey | CredentialError::InvalidId => {
            (StatusCode::BAD_REQUEST, "invalid_request")
        }
        CredentialError::Storage => (
            StatusCode::SERVICE_UNAVAILABLE,
            "credential_storage_unavailable",
        ),
        CredentialError::LoginPending => (StatusCode::CONFLICT, "login_in_progress"),
        CredentialError::LoginFailed => (StatusCode::SERVICE_UNAVAILABLE, "login_unavailable"),
        CredentialError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        CredentialError::Stopping => (StatusCode::SERVICE_UNAVAILABLE, "shutting_down"),
    };
    (
        status,
        Json(ErrorResponse {
            code: code.to_owned(),
            message: error.to_string(),
        }),
    )
        .into_response()
}
