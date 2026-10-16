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
    providers::{ProviderCatalog, SelectAuthRequest, SetApiKeyRequest, StartLoginRequest},
};

use super::chat::{AppState, authorize_request, normalize_rejections};
use crate::{auth::LocalToken, credentials::CredentialError};

pub fn router(token: Arc<LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route(PROVIDERS_PATH, get(statuses))
        .route(
            "/v1/providers/{provider}/api-key",
            put(set_api_key).delete(remove_api_key),
        )
        .route("/v1/providers/{provider}/models", get(discover))
        .route("/v1/providers/codex/accounts", get(accounts))
        .route("/v1/providers/codex/auth", put(select_auth))
        .route(
            "/v1/providers/codex/accounts/{account_id}/logout",
            post(sign_out),
        )
        .route("/v1/providers/codex/login", post(start_login))
        .route("/v1/providers/codex/login/{login_id}", get(login_status))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .route_layer(middleware::from_fn(normalize_rejections))
        .route_layer(middleware::from_fn_with_state(token, authorize_request))
}

#[utoipa::path(
    get,
    path = "/v1/providers",
    tag = "providers",
    responses(
        (status = 200, description = "Credential status for every provider", body = Vec<slop_protocol::providers::ProviderStatus>),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn statuses(State(state): State<Arc<AppState>>) -> Response {
    Json(state.credentials.statuses().await).into_response()
}

#[utoipa::path(
    put,
    path = "/v1/providers/{provider}/api-key",
    tag = "providers",
    params(("provider" = String, Path, description = "Provider id")),
    request_body = SetApiKeyRequest,
    responses(
        (status = 200, description = "Provider status after key replacement", body = slop_protocol::providers::ProviderStatus),
        (status = 400, description = "Unsupported provider or invalid key", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down or storage unavailable", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn set_api_key(
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

#[utoipa::path(
    post,
    path = "/v1/providers/codex/login",
    tag = "providers",
    request_body = StartLoginRequest,
    responses(
        (status = 202, description = "Login flow started", body = slop_protocol::providers::LoginResponse),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 409, description = "Login already in progress", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down or login unavailable", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn start_login(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartLoginRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    match state
        .credentials
        .start_login_for_account(
            request.command_id,
            request.account_id,
            Some(state.runtime.clone()),
        )
        .await
    {
        Ok(login) => (StatusCode::ACCEPTED, Json(login)).into_response(),
        Err(error) => credential_error(error),
    }
}

#[utoipa::path(
    get,
    path = "/v1/providers/codex/login/{login_id}",
    tag = "providers",
    params(("login_id" = String, Path, description = "Login flow id")),
    responses(
        (status = 200, description = "Login flow status", body = slop_protocol::providers::LoginStatus),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Login flow not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn login_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    respond(state.credentials.login_status(&id).await)
}

#[utoipa::path(
    get,
    path = "/v1/providers/codex/accounts",
    tag = "providers",
    responses(
        (status = 200, description = "Known ChatGPT/Codex accounts", body = Vec<slop_protocol::providers::ProviderAccount>),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn accounts(State(state): State<Arc<AppState>>) -> Response {
    Json(state.credentials.accounts().await).into_response()
}

#[utoipa::path(
    put,
    path = "/v1/providers/codex/auth",
    tag = "providers",
    request_body = SelectAuthRequest,
    responses(
        (status = 200, description = "Provider status after auth selection", body = slop_protocol::providers::ProviderStatus),
        (status = 400, description = "Invalid request", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Account not found", body = slop_protocol::ErrorResponse),
        (status = 409, description = "Credentials unavailable or login in progress", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn select_auth(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SelectAuthRequest>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    respond(
        state
            .credentials
            .select_auth(request, state.runtime.clone())
            .await,
    )
}

#[utoipa::path(
    post,
    path = "/v1/providers/codex/accounts/{account_id}/logout",
    tag = "providers",
    params(("account_id" = String, Path, description = "Account id")),
    responses(
        (status = 200, description = "Signed out", body = slop_protocol::providers::SignOutResponse),
        (status = 400, description = "Invalid account id", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Account not found", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn sign_out(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    respond(state.credentials.sign_out(id, state.runtime.clone()).await)
}

#[utoipa::path(
    delete,
    path = "/v1/providers/{provider}/api-key",
    tag = "providers",
    params(("provider" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Provider status after key removal", body = slop_protocol::providers::ProviderStatus),
        (status = 400, description = "Unsupported provider", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 503, description = "Daemon shutting down or storage unavailable", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remove_api_key(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    respond(
        state
            .credentials
            .remove_api_key(provider, state.runtime.clone())
            .await,
    )
}

#[utoipa::path(
    get,
    path = "/v1/providers/{provider}/models",
    tag = "providers",
    params(("provider" = String, Path, description = "Provider id")),
    responses(
        (status = 200, description = "Live provider model IDs enriched with supported public metadata, including reasoning efforts", body = ProviderCatalog),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 502, description = "Provider catalog could not be verified", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub(crate) async fn discover(
    State(state): State<Arc<AppState>>,
    Path(provider): Path<String>,
) -> Response {
    let (client, account_id) = match state
        .credentials
        .discovery_client(&provider, &state.runtime)
        .await
    {
        Ok(client) => client,
        Err(error) => return credential_error(error),
    };
    let ids = match client.list_models().await {
        Ok(ids) if ids.len() <= 512 => ids,
        _ => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(ErrorResponse {
                    code: "provider_discovery_failed".into(),
                    message: "The selected provider catalog could not be verified.".into(),
                }),
            )
                .into_response();
        }
    };
    let observed_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let descriptor = client.descriptor();
    let supported = slop_runtime::providers::catalog::models(descriptor);
    let subscription = client.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription;
    let models = ids
        .into_iter()
        .map(|model| {
            let known = supported.iter().find(|item| item.id == model);
            let executable = known.is_some();
            let caps = client.capabilities(&model);
            slop_protocol::chat::ModelResponse {
                id: format!("{provider}/{model}"),
                provider: provider.clone(),
                display_name: known
                    .map_or_else(|| model.clone(), |item| item.display_name.to_owned()),
                model,
                ready: executable && state.runtime.accepting_work(),
                is_default: false,
                reason: (!executable)
                    .then(|| "Advertised model has no executable adapter mapping.".to_owned()),
                capabilities: slop_protocol::execution::ModelCapabilities {
                    tools: executable && caps.tools,
                    context_window_tokens: caps.context_window_tokens,
                    max_input_tokens: caps.max_input_tokens,
                    reasoning_efforts: if executable {
                        caps.reasoning_efforts
                    } else {
                        Vec::new()
                    },
                    incremental_streaming: executable,
                    max_output_tokens: if subscription { 0 } else { 65_536 },
                    output_token_cap_supported: !subscription,
                },
            }
        })
        .collect();
    Json(ProviderCatalog {
        provider,
        auth_mode: if subscription { "chatgpt" } else { "api_key" }.into(),
        account_id,
        observed_at_unix_ms,
        source: "live".into(),
        models,
    })
    .into_response()
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
        CredentialError::Unavailable => (StatusCode::CONFLICT, "credentials_unavailable"),
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
