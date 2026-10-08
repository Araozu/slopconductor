//! Local HTTP fault-injection server; all credentials are synthetic.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};

pub(super) struct Reply {
    pub status: StatusCode,
    pub content_type: &'static str,
    pub body: String,
    pub location: Option<String>,
}

impl Reply {
    pub fn json(value: serde_json::Value) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "application/json",
            body: value.to_string(),
            location: None,
        }
    }
    pub fn sse(body: String) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "text/event-stream",
            body,
            location: None,
        }
    }
}

pub(super) struct Recorded {
    pub path: String,
    pub headers: HeaderMap,
    pub body: String,
}

struct ServerState {
    replies: VecDeque<Reply>,
    requests: Vec<Recorded>,
}

pub(super) struct Server {
    pub url: String,
    state: Arc<Mutex<ServerState>>,
    task: tokio::task::JoinHandle<()>,
}

async fn handle(State(state): State<Arc<Mutex<ServerState>>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1_000_000).await.unwrap();
    let mut state = state.lock().unwrap();
    state.requests.push(Recorded {
        path: parts.uri.path().to_owned(),
        headers: parts.headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    });
    let reply = state
        .replies
        .pop_front()
        .expect("unexpected extra HTTP request");
    let mut response = (
        reply.status,
        [("content-type", reply.content_type)],
        reply.body,
    )
        .into_response();
    if let Some(location) = reply.location {
        response
            .headers_mut()
            .insert("location", location.parse().unwrap());
    }
    response
}

impl Server {
    pub async fn new(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(ServerState {
            replies: replies.into(),
            requests: Vec::new(),
        }));
        let router = Router::new()
            .fallback(any(handle))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { url, state, task }
    }

    pub fn inspect(&self, inspect: impl FnOnce(&[Recorded])) {
        inspect(&self.state.lock().unwrap().requests);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
