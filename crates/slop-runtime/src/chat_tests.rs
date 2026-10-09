use std::{
    collections::VecDeque,
    convert::Infallible,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::StatusCode,
    response::Response,
    routing::post,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::chat::{self, ChatOutcome, ChatRepository, RepoFuture, RoleKind, TurnWork};
use crate::{
    agent::{RequestCompletion, RequestIntent, ToolIntent},
    providers::inference::InferenceMessage,
    tools::ToolOutcome,
};

struct FixtureRepository {
    work: Mutex<VecDeque<TurnWork>>,
    claim_barrier: Option<Arc<ClaimBarrier>>,
    cancellation: bool,
    outcomes: Mutex<Vec<ChatOutcome>>,
    terminal_failures: AtomicUsize,
    checkpoints: Mutex<Vec<String>>,
    finished: Notify,
    checkpointed: Notify,
}

struct ClaimBarrier {
    started: Notify,
    release: Notify,
}

impl FixtureRepository {
    fn new(cancellation: bool) -> Self {
        Self {
            work: Mutex::new(VecDeque::from([TurnWork {
                turn_id: "turn-1".to_owned(),
                session_id: "session-1".to_owned(),
                requested_model: "opencode-go/glm-5.3-flash".to_owned(),
                max_tokens: None,
                messages: vec![chat::ContextMessage {
                    role: RoleKind::User,
                    text: "Say hello".to_owned(),
                }],

                ..Default::default()
            }])),
            claim_barrier: None,
            cancellation,
            outcomes: Mutex::new(Vec::new()),
            terminal_failures: AtomicUsize::new(0),
            checkpoints: Mutex::new(Vec::new()),
            finished: Notify::new(),
            checkpointed: Notify::new(),
        }
    }

    fn with_works(works: Vec<TurnWork>) -> Self {
        let repository = Self::new(false);
        *repository.work.lock().unwrap() = works.into();
        repository
    }

    fn with_blocked_claim() -> (Self, Arc<ClaimBarrier>) {
        let mut repository = Self::new(false);
        let barrier = Arc::new(ClaimBarrier {
            started: Notify::new(),
            release: Notify::new(),
        });
        repository.claim_barrier = Some(Arc::clone(&barrier));
        (repository, barrier)
    }

    fn fail_terminal_writes(&self, count: usize) {
        self.terminal_failures.store(count, Ordering::Release);
    }

    async fn terminal(&self) -> ChatOutcome {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.finished.notified();
                if let Some(outcome) = self.outcomes.lock().unwrap().first().cloned() {
                    return outcome;
                }
                notified.await;
            }
        })
        .await
        .expect("supervisor should persist terminal state")
    }

    async fn terminal_count(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.finished.notified();
                if self.outcomes.lock().unwrap().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("supervisor should persist all terminal states");
    }

    async fn first_checkpoint(&self) -> String {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.checkpointed.notified();
                if let Some(text) = self.checkpoints.lock().unwrap().first().cloned() {
                    return text;
                }
                notified.await;
            }
        })
        .await
        .expect("supervisor should commit a visible checkpoint")
    }
}

impl ChatRepository for FixtureRepository {
    // This fixture exercises text-only scheduling; tool persistence must fail closed.
    fn begin_request<'a>(&'a self, _: &'a str, _: RequestIntent) -> RepoFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn checkpoint_request<'a>(
        &'a self,
        turn: &'a str,
        _: &'a str,
        message: InferenceMessage,
    ) -> RepoFuture<'a, ()> {
        Box::pin(async move { self.checkpoint_visible(turn, &message.visible_text()).await })
    }
    fn complete_request<'a>(
        &'a self,
        _: &'a str,
        completion: RequestCompletion,
    ) -> RepoFuture<'a, ()> {
        Box::pin(async move {
            assert!(completion.tools.is_empty());
            Ok(())
        })
    }
    fn fail_request<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str) -> RepoFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn start_tool<'a>(&'a self, _: &'a str, _: &'a ToolIntent) -> RepoFuture<'a, ()> {
        Box::pin(async { Err(io::Error::other("text fixture has no tools").into()) })
    }
    fn finish_tool<'a>(
        &'a self,
        _: &'a str,
        _: &'a ToolIntent,
        _: ToolOutcome,
    ) -> RepoFuture<'a, ()> {
        Box::pin(async { Err(io::Error::other("text fixture has no tools").into()) })
    }
    fn claim_next(&self) -> RepoFuture<'_, Option<TurnWork>> {
        Box::pin(async move {
            if let Some(barrier) = &self.claim_barrier {
                barrier.started.notify_one();
                barrier.release.notified().await;
            }
            Ok(self.work.lock().unwrap().pop_front())
        })
    }

    fn checkpoint_visible<'a>(&'a self, _turn_id: &'a str, text: &'a str) -> RepoFuture<'a, ()> {
        Box::pin(async move {
            self.checkpoints.lock().unwrap().push(text.to_owned());
            self.checkpointed.notify_one();
            Ok(())
        })
    }

    fn finish_turn<'a>(&'a self, _turn_id: &'a str, outcome: ChatOutcome) -> RepoFuture<'a, ()> {
        Box::pin(async move {
            if self
                .terminal_failures
                .try_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err(io::Error::other("fixture storage failure").into());
            }
            self.outcomes.lock().unwrap().push(outcome);
            self.finished.notify_one();
            Ok(())
        })
    }

    fn cancellation_requested<'a>(&'a self, _turn_id: &'a str) -> RepoFuture<'a, bool> {
        Box::pin(async move { Ok(self.cancellation) })
    }
}

#[tokio::test]
async fn real_provider_transport_streams_visible_text_and_commits_terminal_response() {
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let expected_text = "x".repeat(40_000);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(
            {
                let expected_text = expected_text.clone();
                move |State(requests): State<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
                requests.lock().unwrap().push(body);
                let first = json!({"id":"fixture","model":"glm-5.3-flash","choices":[{"delta":{"content":expected_text,"reasoning_content":"private thought"},"finish_reason":null}]});
                let terminal = json!({"id":"fixture","model":"glm-5.3-flash","choices":[{"delta":{},"finish_reason":"stop"}]});
                let body = format!("data: {}\n\ndata: {}\n\ndata: [DONE]\n\n", first, terminal);
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "text/event-stream")
                    .body(Body::from(body))
                    .unwrap()
                }
            },
        ),
    )
    .with_state(Arc::clone(&requests));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let repository = Arc::new(FixtureRepository::new(false));
    let runtime = chat::start(
        Arc::clone(&repository),
        Some("fixture-key".to_owned()),
        Some(format!("http://{address}/v1")),
        1,
        None,
        None,
    );
    let mut deltas = runtime.subscribe();
    let outcome = repository.terminal().await;
    runtime.shutdown().await;
    server.abort();

    assert_eq!(outcome.status, chat::ChatStatus::Completed);
    assert_eq!(outcome.text, expected_text);
    assert_eq!(outcome.resolved_model.as_deref(), Some("glm-5.3-flash"));
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(requests.lock().unwrap()[0]["model"], "glm-5.3-flash");
    let mut emitted = Vec::new();
    while let Ok(delta) = deltas.try_recv() {
        emitted.push(delta);
    }
    assert!(emitted.len() >= 3);
    assert!(emitted.iter().all(|delta| delta.text.len() <= 16 * 1024));
    assert_eq!(
        emitted
            .iter()
            .map(|delta| delta.text.as_str())
            .collect::<String>(),
        expected_text
    );
    assert!(
        emitted
            .iter()
            .all(|delta| !delta.text.contains("private thought"))
    );
}

#[tokio::test]
async fn missing_provider_credentials_fail_claimed_turn_without_request() {
    let repository = Arc::new(FixtureRepository::new(false));
    let runtime = chat::start(Arc::clone(&repository), None, None, 1, None, None);
    let outcome = repository.terminal().await;
    runtime.shutdown().await;

    assert_eq!(outcome.status, chat::ChatStatus::Failed);
    assert_eq!(
        outcome.error_code.as_deref(),
        Some("provider_auth_required")
    );
}

#[tokio::test]
async fn durable_cancel_before_dispatch_avoids_provider_request() {
    let requests = Arc::new(Mutex::new(0usize));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                |State(requests): State<Arc<Mutex<usize>>>, _body: Json<Value>| async move {
                    *requests.lock().unwrap() += 1;
                    StatusCode::OK
                },
            ),
        )
        .with_state(Arc::clone(&requests));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let repository = Arc::new(FixtureRepository::new(true));
    let runtime = chat::start(
        Arc::clone(&repository),
        Some("fixture-key".to_owned()),
        Some(format!("http://{address}/v1")),
        1,
        None,
        None,
    );
    let outcome = repository.terminal().await;
    runtime.shutdown().await;
    server.abort();

    assert_eq!(outcome.status, chat::ChatStatus::Canceled);
    assert_eq!(*requests.lock().unwrap(), 0);
}

#[tokio::test]
async fn shutdown_checkpoints_visible_text_and_discards_interrupted_thinking() {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|_body: Json<Value>| async move {
            let chunks = futures_util::stream::unfold(0, |index| async move {
                if index == 0 {
                    Some((
                        Ok::<Bytes, Infallible>(Bytes::from(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"partial visible\"}}]}\n\n",
                        )),
                        1,
                    ))
                } else {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    None
                }
            });
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(chunks))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let repository = Arc::new(FixtureRepository::new(false));
    let runtime = chat::start(
        Arc::clone(&repository),
        Some("fixture-key".to_owned()),
        Some(format!("http://{address}/v1")),
        1,
        None,
        None,
    );

    let checkpoint = repository.first_checkpoint().await;
    runtime.shutdown().await;
    server.abort();

    assert_eq!(checkpoint, "partial visible");
    let outcome = repository.outcomes.lock().unwrap()[0].clone();
    assert_eq!(outcome.status, chat::ChatStatus::Interrupted);
    assert!(outcome.text.is_empty());
}

#[tokio::test]
async fn global_concurrency_cap_holds_second_provider_request_until_slot_opens() {
    struct Gates {
        requests: AtomicUsize,
        permits: [tokio::sync::Semaphore; 2],
    }
    let gates = Arc::new(Gates {
        requests: AtomicUsize::new(0),
        permits: [
            tokio::sync::Semaphore::new(0),
            tokio::sync::Semaphore::new(0),
        ],
    });
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                |State(gates): State<Arc<Gates>>, _body: Json<Value>| async move {
                    let index = gates.requests.fetch_add(1, Ordering::AcqRel);
                    gates.permits[index.min(1)]
                        .acquire()
                        .await
                        .unwrap()
                        .forget();
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(Body::from(concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"done\"}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        )))
                        .unwrap()
                },
            ),
        )
        .with_state(Arc::clone(&gates));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut first = FixtureRepository::new(false).work.lock().unwrap()[0].clone();
    first.session_id = "session-one".to_owned();
    let mut second = first.clone();
    second.turn_id = "turn-two".to_owned();
    second.session_id = "session-two".to_owned();
    let repository = Arc::new(FixtureRepository::with_works(vec![first, second]));
    let runtime = chat::start(
        Arc::clone(&repository),
        Some("fixture-key".to_owned()),
        Some(format!("http://{address}/v1")),
        1,
        None,
        None,
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while gates.requests.load(Ordering::Acquire) < 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first provider call should start");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(gates.requests.load(Ordering::Acquire), 1);
    gates.permits[0].add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while gates.requests.load(Ordering::Acquire) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("second provider call should start after the first slot is released");
    gates.permits[1].add_permits(1);
    repository.terminal_count(2).await;
    runtime.shutdown().await;
    server.abort();
    assert_eq!(gates.requests.load(Ordering::Acquire), 2);
    assert!(
        repository
            .outcomes
            .lock()
            .unwrap()
            .iter()
            .all(|outcome| outcome.status == chat::ChatStatus::Completed)
    );
}

#[tokio::test]
async fn failed_terminal_commit_stops_claiming_without_replaying_provider_work() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(
                |State(requests): State<Arc<AtomicUsize>>, _body: Json<Value>| async move {
                    requests.fetch_add(1, Ordering::AcqRel);
                    Response::builder()
                        .status(StatusCode::OK)
                        .header("content-type", "text/event-stream")
                        .body(Body::from(concat!(
                            "data: {\"choices\":[{\"delta\":{\"content\":\"once\"}}]}\n\n",
                            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                            "data: [DONE]\n\n"
                        )))
                        .unwrap()
                },
            ),
        )
        .with_state(Arc::clone(&requests));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut first = FixtureRepository::new(false).work.lock().unwrap()[0].clone();
    first.session_id = "session-one".to_owned();
    let mut second = first.clone();
    second.turn_id = "turn-two".to_owned();
    second.session_id = "session-two".to_owned();
    let repository = Arc::new(FixtureRepository::with_works(vec![first, second]));
    repository.fail_terminal_writes(5);
    let runtime = chat::start(
        Arc::clone(&repository),
        Some("fixture-key".to_owned()),
        Some(format!("http://{address}/v1")),
        1,
        None,
        None,
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while runtime.accepting_work() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("repeated terminal storage failure should stop the supervisor");
    runtime.shutdown().await;
    server.abort();

    assert_eq!(requests.load(Ordering::Acquire), 1);
    assert!(
        repository
            .outcomes
            .lock()
            .unwrap()
            .iter()
            .all(|outcome| outcome.status == chat::ChatStatus::Interrupted)
    );
}

#[tokio::test]
async fn shutdown_waits_for_an_inflight_durable_claim_before_interrupting_it() {
    let (fixture, barrier) = FixtureRepository::with_blocked_claim();
    let repository = Arc::new(fixture);
    let runtime = chat::start(Arc::clone(&repository), None, None, 1, None, None);

    tokio::time::timeout(Duration::from_secs(5), barrier.started.notified())
        .await
        .expect("scheduler should begin the durable claim");

    let runtime_to_stop = runtime.clone();
    let shutdown = tokio::spawn(async move { runtime_to_stop.shutdown().await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !shutdown.is_finished(),
        "shutdown must wait for the already-submitted durable claim"
    );

    barrier.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), shutdown)
        .await
        .expect("scheduler should finish shutdown")
        .expect("shutdown task should not panic");

    let outcome = repository.terminal().await;
    assert_eq!(outcome.status, chat::ChatStatus::Interrupted);
    assert_eq!(outcome.text, "");
}

#[test]
fn provider_endpoint_override_restricts_plain_http_and_url_parts() {
    use crate::providers::opencode_go::OpencodeGoClient;

    assert!(OpencodeGoClient::validate_base_url("http://127.0.0.1:9000/v1").is_ok());
    assert!(OpencodeGoClient::validate_base_url("http://localhost:9000/v1").is_ok());
    assert!(OpencodeGoClient::validate_base_url("https://provider.example/v1").is_ok());
    for invalid in [
        "http://provider.example/v1",
        "http://user@localhost:9000/v1",
        "http://@localhost:9000/v1",
        "http://127.0.0.1:9000/v1?token=secret",
        "http://127.0.0.1:9000/v1#frag",
    ] {
        assert!(
            OpencodeGoClient::validate_base_url(invalid).is_err(),
            "{invalid}"
        );
    }
}
