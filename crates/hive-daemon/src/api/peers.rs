//! Administrator-only peer registry and explicit one-hop public operations.
use super::chat::{self, AppState, EventQuery, PageQuery, error, storage_response, store_error};
use crate::{
    peers::{self, Outbound, PeerService},
    storage::StoreError,
};
use axum::{
    Json, Router,
    body::{Body, BodyDataStream, Bytes},
    extract::{Path, Query, State},
    http::{HeaderValue, Request, StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures_util::{StreamExt, stream};
use hive_client::ClientError;
use hive_protocol::{ErrorResponse, HealthResponse, NodeResponse, chat::*, peers::*};
use std::{convert::Infallible, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, watch};

pub fn router(token: Arc<crate::auth::LocalToken>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/v1/peer-grants", get(grants))
        .route("/v1/peer-grants/{id}", put(create_grant))
        .route("/v1/peer-grants/{id}/revoke", post(revoke_grant))
        .route("/v1/peers", get(list_peers))
        .route("/v1/peers/{id}", put(register_peer).delete(remove_peer))
        .route("/v1/peers/{id}/probe", get(probe))
        .route("/v1/peers/{peer_id}/health", get(remote_health))
        .route("/v1/peers/{peer_id}/node", get(remote_node))
        .route("/v1/peers/{peer_id}/sessions", get(remote_sessions))
        .route(
            "/v1/peers/{peer_id}/sessions/{session_id}",
            get(remote_session),
        )
        .route(
            "/v1/peers/{peer_id}/sessions/{session_id}/messages",
            get(remote_history).post(remote_send),
        )
        .route(
            "/v1/peers/{peer_id}/sessions/{session_id}/instructions",
            get(remote_instructions),
        )
        .route(
            "/v1/peers/{peer_id}/sessions/{session_id}/events",
            get(remote_events),
        )
        .route("/v1/peers/{peer_id}/turns/{turn_id}", get(remote_turn))
        .route(
            "/v1/peers/{peer_id}/turns/{turn_id}/cancel",
            post(remote_cancel),
        )
        .route(
            "/v1/peers/{peer_id}/turns/{turn_id}/pause",
            post(remote_pause),
        )
        .route(
            "/v1/peers/{peer_id}/turns/{turn_id}/resume",
            post(remote_resume),
        )
        .route(
            "/v1/peers/{peer_id}/messages/{message_id}",
            get(remote_message),
        )
        .route("/v1/peers/{peer_id}/tools/{tool_id}", get(remote_tool))
        .route_layer(middleware::from_fn(chat::normalize_rejections))
        .route_layer(middleware::from_fn_with_state(
            token,
            chat::authorize_request,
        ))
}

/// Same daemon/runtime, separate listener and explicit route allowlist.
pub fn inbound_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/node", get(peer_node))
        .route("/v1/sessions", get(chat::sessions))
        .route("/v1/sessions/{session_id}", get(chat::session))
        .route(
            "/v1/sessions/{session_id}/messages",
            get(chat::messages).post(peer_send),
        )
        .route("/v1/sessions/{session_id}/events", get(chat::events))
        .route(
            "/v1/sessions/{session_id}/instructions",
            get(chat::steering_instructions),
        )
        .route("/v1/turns/{turn_id}", get(chat::turn))
        .route("/v1/turns/{turn_id}/cancel", post(chat::cancel_turn))
        .route("/v1/turns/{turn_id}/pause", post(chat::pause_turn))
        .route("/v1/turns/{turn_id}/resume", post(chat::resume_turn))
        .route("/v1/messages/{id}", get(chat::message))
        .route("/v1/tools/{id}", get(chat::tool_invocation))
        .route_layer(middleware::from_fn(chat::normalize_rejections))
        .route_layer(middleware::from_fn_with_state(state.clone(), inbound_auth))
        .route("/v1/health", get(peer_health))
        .with_state(state)
}
async fn peer_health() -> Json<HealthResponse> {
    Json(HealthResponse {
        service: hive_protocol::SERVICE_NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        api_version: hive_protocol::API_VERSION,
        capabilities: vec![
            "daemon-peering",
            "sessions",
            "text-chat",
            "structured-messages",
            "execution-steering",
            "turn-pause-resume",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    })
}
async fn peer_node(State(state): State<Arc<AppState>>) -> Response {
    storage_response(state.store.node().await)
}
async fn peer_send(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    if request.model.is_some() || request.settings.is_some() {
        return error(
            StatusCode::BAD_REQUEST,
            "peer_settings_unsupported",
            "Peer steering uses the session's existing model and settings.",
        );
    }
    chat::send_message(State(state), Path(id), Json(request)).await
}
fn owner(mut response: Response, node: &str) -> Response {
    if let Ok(value) = HeaderValue::from_str(node) {
        response.headers_mut().insert("x-slop-owner-node-id", value);
    }
    response
}
fn network_error(error_value: ClientError) -> Response {
    match error_value {
        ClientError::Api {
            status,
            code,
            message,
        } => error(
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            &code,
            &message,
        ),
        ClientError::Unauthorized { .. } => error(
            StatusCode::UNAUTHORIZED,
            "peer_unauthorized",
            "The owner rejected the peer credential.",
        ),
        ClientError::DeliveryUncertain(_) => error(
            StatusCode::BAD_GATEWAY,
            "peer_delivery_unknown",
            "The owner may have accepted the command. Explicitly retry the original command ID and payload to retrieve its receipt.",
        ),
        ClientError::IncompatibleApi { .. }
        | ClientError::UnexpectedService(_)
        | ClientError::UnsupportedCapability(_) => error(
            StatusCode::BAD_GATEWAY,
            "peer_incompatible",
            "The peer does not support the required API or capability.",
        ),
        _ => error(
            StatusCode::BAD_GATEWAY,
            "peer_unreachable",
            "The peer request failed or returned invalid data.",
        ),
    }
}
fn valid_id(id: &str) -> Result<(), Box<Response>> {
    if peers::identifier(id) {
        Ok(())
    } else {
        Err(Box::new(error(
            StatusCode::BAD_REQUEST,
            "invalid_peer_id",
            "Use 1-128 ASCII letters, digits, underscores or hyphens.",
        )))
    }
}
async fn inbound_auth(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let changed = state.peers.changed.subscribe();
    let mut headers = request.headers().get_all(AUTHORIZATION).iter();
    let digest = headers
        .next()
        .and_then(|v| {
            if headers.next().is_some() {
                None
            } else {
                v.to_str().ok()
            }
        })
        .and_then(crate::auth::bearer_token)
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(peers::token_digest);
    let Some(digest) = digest else {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid peer bearer is required.",
        );
    };
    let grant = match state.store.authenticate_peer(digest.clone()).await {
        Ok(grant) => grant,
        Err(StoreError::NotFound) => {
            return error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "The peer grant is unknown or revoked.",
            );
        }
        Err(error) => return store_error(error),
    };
    // The gateway binds each authenticated request to the registered owner,
    // closing the probe/dispatch race if the service restarts with a new node.
    let mut expected = request
        .headers()
        .get_all("x-slop-expected-owner-node-id")
        .iter();
    if let Some(value) = expected.next()
        && (expected.next().is_some() || value.to_str().ok() != Some(grant.owner_node_id.as_str()))
    {
        return error(
            StatusCode::CONFLICT,
            "peer_identity_mismatch",
            "The request targets a different owner node.",
        );
    }
    if request.method() != axum::http::Method::GET && grant.scope != PeerScope::Control {
        return error(
            StatusCode::FORBIDDEN,
            "peer_read_only",
            "This peer grant permits read access only.",
        );
    }
    let streaming = request.uri().path().ends_with("/events");
    let permits = match state
        .peers
        .admit(format!("in-{}-{streaming}", grant.id), streaming)
    {
        Ok(p) => p,
        Err(code) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                code,
                "Peer connection capacity is unavailable.",
            );
        }
    };
    let response = owner(next.run(request).await, &grant.owner_node_id);
    if streaming {
        guard(
            response,
            state.peers.clone(),
            changed,
            Some(digest),
            None,
            permits,
        )
    } else {
        response
    }
}
struct GuardedBody {
    body: BodyDataStream,
    service: Arc<PeerService>,
    changed: watch::Receiver<()>,
    grant: Option<String>,
    peer: Option<(String, String)>,
    _permits: Vec<OwnedSemaphorePermit>,
}
fn guard(
    response: Response,
    service: Arc<PeerService>,
    changed: watch::Receiver<()>,
    grant: Option<String>,
    peer: Option<(String, String)>,
    permits: Vec<OwnedSemaphorePermit>,
) -> Response {
    let (parts, body) = response.into_parts();
    let feed = GuardedBody {
        body: body.into_data_stream(),
        service,
        changed,
        grant,
        peer,
        _permits: permits,
    };
    let body = Body::from_stream(stream::unfold(feed, |mut feed| async move {
        loop {
            if feed.service.is_closing() {
                return None;
            }
            tokio::select! { biased;
                result=feed.changed.changed()=>{
                    if result.is_err() || feed.service.is_closing() {return None;}
                    if let Some(digest)=&feed.grant && feed.service.store.authenticate_peer(digest.clone()).await.is_err() {return None;}
                    if let Some((id,digest))=&feed.peer {
                        match feed.service.store.peer(id.clone()).await {Ok((p,d)) if !p.removed && &d==digest=>{},_=>return None}
                    }
                },
                chunk=feed.body.next()=>return chunk.map(|chunk|(chunk,feed)),
            }
        }
    }));
    Response::from_parts(parts, body)
}
struct Remote {
    outbound: Arc<Outbound>,
    _permits: Vec<OwnedSemaphorePermit>,
    changed: watch::Receiver<()>,
}
async fn connect(
    state: &Arc<AppState>,
    id: &str,
    streaming: bool,
) -> Result<Remote, Box<Response>> {
    valid_id(id)?;
    let changed = state.peers.changed.subscribe();
    let permits = state
        .peers
        .admit(format!("out-{id}-{streaming}"), streaming)
        .map_err(|code| {
            Box::new(error(
                StatusCode::TOO_MANY_REQUESTS,
                code,
                "Peer connection capacity is unavailable.",
            ))
        })?;
    let outbound = state
        .peers
        .outbound(id)
        .await
        .map_err(|e| Box::new(network_error(e)))?;
    state
        .peers
        .verify(&outbound)
        .await
        .map_err(|e| Box::new(network_error(e)))?;
    Ok(Remote {
        outbound,
        _permits: permits,
        changed,
    })
}
fn result<T: serde::Serialize>(remote: &Remote, value: Result<T, ClientError>) -> Response {
    owner(
        match value {
            Ok(v) => Json(v).into_response(),
            Err(e) => network_error(e),
        },
        &remote.outbound.peer.owner_node_id,
    )
}

fn accepted_result<T: serde::Serialize>(
    remote: &Remote,
    value: Result<T, ClientError>,
) -> Response {
    let mut response = result(remote, value);
    if response.status().is_success() {
        *response.status_mut() = StatusCode::ACCEPTED;
    }
    response
}

#[utoipa::path(
    get, path = "/v1/peer-grants", tag = "peers",
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = Vec<PeerGrantResponse>), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn grants(State(state): State<Arc<AppState>>) -> Response {
    storage_response(state.store.peer_grants().await)
}

#[utoipa::path(
    put, path = "/v1/peer-grants/{id}", tag = "peers",
    params(("id" = String, Path)),
    request_body = CreatePeerGrantRequest,
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = PeerGrantResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn create_grant(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<CreatePeerGrantRequest>,
) -> Response {
    if let Err(e) = valid_id(&id) {
        return *e;
    }
    let Some(digest) = peers::token_digest(&request.token) else {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_peer_token",
            "Peer tokens must be 64 lowercase hexadecimal digits.",
        );
    };
    if request.label.trim().is_empty()
        || request.label.len() > 128
        || request.label.chars().any(char::is_control)
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_label",
            "A peer label must contain 1-128 non-control characters.",
        );
    }
    storage_response(
        state
            .store
            .put_peer_grant(id, request.label, request.scope, digest)
            .await,
    )
}

#[utoipa::path(
    post, path = "/v1/peer-grants/{id}/revoke", tag = "peers",
    params(("id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = PeerGrantResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn revoke_grant(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let result = state.store.revoke_peer_grant(id).await;
    state.peers.notify();
    storage_response(result)
}

#[utoipa::path(
    get, path = "/v1/peers", tag = "peers",
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = Vec<PeerResponse>), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn list_peers(State(state): State<Arc<AppState>>) -> Response {
    storage_response(state.store.peers().await)
}

#[utoipa::path(
    put, path = "/v1/peers/{id}", tag = "peers",
    params(("id" = String, Path)),
    request_body = RegisterPeerRequest,
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = PeerResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn register_peer(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<RegisterPeerRequest>,
) -> Response {
    if let Err(e) = valid_id(&id) {
        return *e;
    }
    if !peers::identifier(&request.owner_node_id)
        || !peers::valid_origin(&request.origin)
        || peers::token_digest(&request.token).is_none()
    {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_peer",
            "Use a trusted HTTPS origin, explicit owner node ID, and a 64-digit lowercase token. HTTP is limited to literal loopback fixtures.",
        );
    }
    storage_response(state.peers.register(id, request).await)
}

#[utoipa::path(
    delete, path = "/v1/peers/{id}", tag = "peers",
    params(("id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = PeerResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remove_peer(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let result = state.store.remove_peer(id).await;
    state.peers.notify();
    storage_response(result)
}

#[utoipa::path(
    get, path = "/v1/peers/{id}/probe", tag = "peers",
    params(("id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = PeerProbeResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn probe(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let r = match connect(&state, &id, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(
        &r,
        r.outbound
            .client
            .node()
            .await
            .map(|node| PeerProbeResponse {
                peer: r.outbound.peer.clone(),
                node,
            }),
    )
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/health", tag = "peers",
    params(("peer_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = HealthResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_health(
    State(state): State<Arc<AppState>>,
    Path(peer): Path<String>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.health().await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/node", tag = "peers",
    params(("peer_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = NodeResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_node(
    State(state): State<Arc<AppState>>,
    Path(peer): Path<String>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.node().await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/sessions", tag = "peers",
    params(("peer_id" = String, Path), ("after" = Option<u64>, Query), ("limit" = Option<u32>, Query)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = Page<SessionResponse>), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_sessions(
    State(state): State<Arc<AppState>>,
    Path(peer): Path<String>,
    Query(query): Query<PageQuery>,
) -> Response {
    if let Err(e) = chat::checked_limit(query.limit) {
        return e.into_response();
    }
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(
        &r,
        r.outbound
            .client
            .list_sessions(query.after, query.limit.map(|v| v as u32))
            .await,
    )
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/sessions/{session_id}", tag = "peers",
    params(("peer_id" = String, Path), ("session_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = SessionResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_session(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.session(&id).await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/sessions/{session_id}/messages", tag = "peers",
    params(("peer_id" = String, Path), ("session_id" = String, Path), ("after" = Option<u64>, Query), ("limit" = Option<u32>, Query)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = Page<MessageResponse>), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_history(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Query(query): Query<PageQuery>,
) -> Response {
    if let Err(e) = chat::checked_limit(query.limit) {
        return e.into_response();
    }
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(
        &r,
        r.outbound
            .client
            .history(&id, query.after, query.limit.map(|v| v as u32))
            .await,
    )
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/sessions/{session_id}/instructions", tag = "peers",
    params(("peer_id" = String, Path), ("session_id" = String, Path), ("after" = Option<u64>, Query), ("limit" = Option<u32>, Query)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = Page<SteeringInstructionResponse>), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_instructions(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Query(query): Query<PageQuery>,
) -> Response {
    if let Err(e) = chat::checked_limit(query.limit) {
        return e.into_response();
    }
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(
        &r,
        r.outbound
            .client
            .steering_instructions(&id, query.after, query.limit.map(|v| v as u32))
            .await,
    )
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/turns/{turn_id}", tag = "peers",
    params(("peer_id" = String, Path), ("turn_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = TurnResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_turn(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.turn(&id).await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/messages/{message_id}", tag = "peers",
    params(("peer_id" = String, Path), ("message_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = MessageResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_message(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.message(&id).await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/tools/{tool_id}", tag = "peers",
    params(("peer_id" = String, Path), ("tool_id" = String, Path)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = hive_protocol::execution::ToolInvocationResponse), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_tool(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.tool_invocation(&id).await)
}

#[utoipa::path(
    post, path = "/v1/peers/{peer_id}/sessions/{session_id}/messages", tag = "peers",
    params(("peer_id" = String, Path), ("session_id" = String, Path)),
    request_body = SendMessageRequest,
    responses((status = 202, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = CommandReceipt), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_send(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Json(request): Json<SendMessageRequest>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    accepted_result(&r, r.outbound.client.send_message(&id, &request).await)
}

#[utoipa::path(
    post, path = "/v1/peers/{peer_id}/turns/{turn_id}/cancel", tag = "peers",
    params(("peer_id" = String, Path), ("turn_id" = String, Path)),
    request_body = CancelTurnRequest,
    responses((status = 202, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = CommandReceipt), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_cancel(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Json(request): Json<CancelTurnRequest>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    accepted_result(&r, r.outbound.client.cancel_turn(&id, &request).await)
}

#[utoipa::path(
    post, path = "/v1/peers/{peer_id}/turns/{turn_id}/pause", tag = "peers",
    params(("peer_id" = String, Path), ("turn_id" = String, Path)),
    request_body = TurnControlRequest,
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = CommandReceipt), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_pause(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Json(request): Json<TurnControlRequest>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.pause_turn(&id, &request).await)
}

#[utoipa::path(
    post, path = "/v1/peers/{peer_id}/turns/{turn_id}/resume", tag = "peers",
    params(("peer_id" = String, Path), ("turn_id" = String, Path)),
    request_body = TurnControlRequest,
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = CommandReceipt), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_resume(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Json(request): Json<TurnControlRequest>,
) -> Response {
    let r = match connect(&state, &peer, false).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    result(&r, r.outbound.client.resume_turn(&id, &request).await)
}

#[utoipa::path(
    get, path = "/v1/peers/{peer_id}/sessions/{session_id}/events", tag = "peers",
    params(("peer_id" = String, Path), ("session_id" = String, Path), ("after" = Option<u64>, Query), ("follow" = Option<bool>, Query)),
    responses((status = 200, description = "Peer operation result; forwarded results include x-slop-owner-node-id", body = EventFrame, content_type = "application/x-ndjson"), (status = 400, description = "Invalid request or cursor", body = ErrorResponse), (status = 401, description = "Credential rejected", body = ErrorResponse), (status = 404, description = "Peer or owner record not found", body = ErrorResponse), (status = 413, description = "Request exceeds byte limit", body = ErrorResponse), (status = 429, description = "Peer admission limit reached", body = ErrorResponse), (status = 503, description = "Local storage or shutdown unavailable", body = ErrorResponse), (status = 403, description = "Scope denied", body = ErrorResponse), (status = 409, description = "Identity or registration conflict", body = ErrorResponse), (status = 502, description = "Peer unavailable or mutation delivery unknown", body = ErrorResponse)),
    security(("bearer_auth" = []))
)]
pub(crate) async fn remote_events(
    State(state): State<Arc<AppState>>,
    Path((peer, id)): Path<(String, String)>,
    Query(query): Query<EventQuery>,
) -> Response {
    let r = match connect(&state, &peer, true).await {
        Ok(r) => r,
        Err(e) => return *e,
    };
    let events = match r
        .outbound
        .client
        .events(&id, query.after.unwrap_or(0), query.follow.unwrap_or(false))
        .await
    {
        Ok(events) => events,
        Err(e) => return network_error(e),
    };
    let body = Body::from_stream(stream::unfold(events, |mut events| async move {
        let frame = events.next_frame().await.ok()??;
        let mut bytes = serde_json::to_vec(&frame).ok()?;
        bytes.push(b'\n');
        Some((Ok::<Bytes, Infallible>(Bytes::from(bytes)), events))
    }));
    let mut response = Response::new(body);
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    let response = owner(response, &r.outbound.peer.owner_node_id);
    guard(
        response,
        state.peers.clone(),
        r.changed,
        None,
        Some((peer, r.outbound.digest.clone())),
        r._permits,
    )
}
