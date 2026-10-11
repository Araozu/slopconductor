use super::*;

pub(in crate::api) async fn authorize_request(
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

pub(in crate::api) async fn normalize_rejections(request: Request<Body>, next: Next) -> Response {
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

pub(in crate::api) fn checked_limit(limit: Option<usize>) -> Result<usize, ApiError> {
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

pub(in crate::api) fn accepted<T: serde::Serialize>(result: Result<T, StoreError>) -> Response {
    match result {
        Ok(value) => (StatusCode::ACCEPTED, Json(value)).into_response(),
        Err(error) => store_error(error),
    }
}

pub(in crate::api) fn storage_response<T: serde::Serialize>(
    result: Result<T, StoreError>,
) -> Response {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(error) => store_error(error),
    }
}

pub(in crate::api) fn store_error(store_error: StoreError) -> Response {
    match store_error {
        StoreError::Git(git) => error(
            match git.code {
                "git_unavailable" | "git_timeout" => StatusCode::SERVICE_UNAVAILABLE,
                "git_output_limit" => StatusCode::PAYLOAD_TOO_LARGE,
                "project_changed" | "workspace_path_denied" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_REQUEST,
            },
            git.code,
            "The Git operation failed; inspect the project or workspace before retrying.",
        ),
        StoreError::Busy => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_busy",
            "Local storage is busy; retry using the same command id.",
        ),
        StoreError::Invalid => error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "The request is invalid.",
        ),
        StoreError::Unsupported(field) => {
            error(StatusCode::BAD_REQUEST, "unsupported_capability", field)
        }
        StoreError::ActiveTurnOverride => error(
            StatusCode::BAD_REQUEST,
            "active_turn_override",
            "An instruction attached to an active turn cannot change its frozen model or settings.",
        ),
        StoreError::Preflight(code, message) => error(StatusCode::BAD_REQUEST, code, message),
        StoreError::Limit => error(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_limit",
            "The configured queue or context limit has been reached.",
        ),
        StoreError::NotFound => error(
            StatusCode::NOT_FOUND,
            "not_found",
            "The requested record does not exist.",
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

pub(in crate::api) fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorResponse {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    )
        .into_response()
}

pub(in crate::api) struct ApiError {
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
