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

async fn login_status(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    respond(state.credentials.login_status(&id).await)
}

async fn accounts(State(state): State<Arc<AppState>>) -> Response {
    Json(state.credentials.accounts().await).into_response()
}

async fn select_auth(
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

async fn sign_out(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    if !state.accepting.load(Ordering::Acquire) {
        return credential_error(CredentialError::Stopping);
    }
    respond(state.credentials.sign_out(id, state.runtime.clone()).await)
}

async fn remove_api_key(
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

async fn discover(State(state): State<Arc<AppState>>, Path(provider): Path<String>) -> Response {
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
    let subscription = client.auth_mode() == slop_runtime::providers::AuthMode::ChatGptSubscription;
    let models = ids
        .into_iter()
        .map(|model| {
            let known = descriptor.models().iter().find(|item| item.id == model);
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
