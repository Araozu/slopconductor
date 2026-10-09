//! Daemon-owned, bounded text-chat scheduling.
//!
//! The supervisor claims durable turns lazily and runs inference on the shared
//! Tokio runtime. A request connection never owns or waits for execution.

use std::{
    collections::HashMap,
    error::Error,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::providers::{ProviderClient, ProviderError, Usage, opencode_go::OpencodeGoClient};
use crate::{
    agent::{RequestCompletion, RequestIntent, ToolIntent},
    providers::inference::{GenerationSettings, InferenceMessage},
    tools::{ToolOutcome, ToolService, WorkspacePolicy},
};

pub const DEFAULT_MODEL: &str = "opencode-go/glm-5.3-flash";
pub const DEFAULT_OUTPUT_TOKENS: u32 = 4_096;
pub const DEFAULT_CONCURRENCY: usize = 4;
const CLAIM_POLL: Duration = Duration::from_millis(100);

pub type RepoError = Box<dyn Error + Send + Sync>;
pub type RepoFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RepoError>> + Send + 'a>>;

/// Storage boundary for accepted chat turns. Implementations own transaction
/// semantics and wire-independent persistence; the runtime never sees SQL/DTOs.
pub trait ChatRepository: Send + Sync + 'static {
    fn begin_request<'a>(&'a self, turn_id: &'a str, intent: RequestIntent) -> RepoFuture<'a, ()>;
    fn checkpoint_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        message: InferenceMessage,
    ) -> RepoFuture<'a, ()>;
    /// Commit the completed message and every tool intent atomically before dispatch.
    fn complete_request<'a>(
        &'a self,
        turn_id: &'a str,
        completion: RequestCompletion,
    ) -> RepoFuture<'a, ()>;
    fn fail_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        code: &'a str,
    ) -> RepoFuture<'a, ()>;
    fn start_tool<'a>(&'a self, turn_id: &'a str, intent: &'a ToolIntent) -> RepoFuture<'a, ()>;
    fn finish_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a ToolIntent,
        outcome: ToolOutcome,
    ) -> RepoFuture<'a, ()>;
    /// Atomically claim the next eligible turn and mark it running.
    fn claim_next(&self) -> RepoFuture<'_, Option<TurnWork>>;
    /// Persist bounded visible assistant text as an incomplete checkpoint.
    fn checkpoint_visible<'a>(&'a self, turn_id: &'a str, text: &'a str) -> RepoFuture<'a, ()>;
    /// Atomically record the terminal assistant message, usage, outcome, and
    /// semantic event before the caller considers the turn complete.
    fn finish_turn<'a>(&'a self, turn_id: &'a str, outcome: ChatOutcome) -> RepoFuture<'a, ()>;
    /// Read the durable cancellation bit. Runtime polls it while inference is
    /// in flight; there is no automatic replay after interruption.
    fn cancellation_requested<'a>(&'a self, turn_id: &'a str) -> RepoFuture<'a, bool>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    System,
    Developer,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMessage {
    pub role: RoleKind,
    pub text: String,
}

/// Durable input chosen when the repository claims a turn. Later queued user
/// messages must never be appended to this context after claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnWork {
    pub turn_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub max_tokens: Option<u32>,
    pub messages: Vec<ContextMessage>,
    pub history: Vec<InferenceMessage>,
    pub settings: GenerationSettings,
    pub requested_settings: GenerationSettings,
    pub execution: Option<WorkspacePolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStatus {
    Completed,
    Incomplete,
    Interrupted,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatOutcome {
    pub text: String,
    pub resolved_model: Option<String>,
    pub usage: Usage,
    pub status: ChatStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatDelta {
    pub session_id: String,
    pub turn_id: String,
    pub text: String,
    pub message_id: Option<String>,
    pub block_id: Option<String>,
    pub request_id: Option<String>,
    pub invocation_id: Option<String>,
    pub stream_id: String,
    pub chunk_index: u64,
    pub kind: String,
}

#[derive(Clone)]
pub struct ChatRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    providers: tokio::sync::watch::Sender<HashMap<String, Arc<dyn ProviderClient>>>,
    go_base_url: Option<String>,
    scheduler_healthy: Arc<AtomicBool>,
    default_model: String,
    max_tokens: u32,
    shutdown: Option<tokio::sync::watch::Sender<bool>>,
    scheduler: Mutex<Option<tokio::task::JoinHandle<()>>>,
    tools: Arc<ToolService>,
}

/// Starts the daemon-owned scheduler. `base_url` is an optional explicitly
/// trusted provider endpoint; it must already have passed daemon config checks.
#[must_use]
pub fn start<R: ChatRepository>(
    repository: Arc<R>,
    api_key: Option<String>,
    base_url: Option<String>,
    concurrency: usize,
    default_model: Option<String>,
    max_tokens: Option<u32>,
) -> ChatRuntime {
    start_with_tools(
        repository,
        api_key,
        base_url,
        concurrency,
        default_model,
        max_tokens,
        Arc::new(ToolService::disabled()),
    )
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn start_with_tools<R: ChatRepository>(
    repository: Arc<R>,
    api_key: Option<String>,
    base_url: Option<String>,
    concurrency: usize,
    default_model: Option<String>,
    max_tokens: Option<u32>,
    tools: Arc<ToolService>,
) -> ChatRuntime {
    let client = api_key.as_deref().and_then(|key| {
        use crate::providers::opencode_go::OpencodeGoClient;
        let result = match base_url.as_deref() {
            Some(url) => OpencodeGoClient::new_with_base_url(key, url),
            None => OpencodeGoClient::new(key),
        };
        result.ok()
    });
    let providers = client
        .map(|client| {
            let provider: Arc<dyn ProviderClient> = Arc::new(client);
            HashMap::from([(provider.descriptor().id().as_str().to_owned(), provider)])
        })
        .unwrap_or_default();
    start_with_provider_clients(
        repository,
        providers,
        base_url,
        concurrency,
        default_model,
        max_tokens,
        tools,
    )
}

/// Starts the common provider scheduler with already validated authenticated clients.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn start_with_provider_clients<R: ChatRepository>(
    repository: Arc<R>,
    providers: HashMap<String, Arc<dyn ProviderClient>>,
    base_url: Option<String>,
    concurrency: usize,
    default_model: Option<String>,
    max_tokens: Option<u32>,
    tools: Arc<ToolService>,
) -> ChatRuntime {
    let (deltas, _) = tokio::sync::broadcast::channel(128);
    let default_model = default_model.unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    let max_tokens = max_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
    let (providers, provider_receiver) = tokio::sync::watch::channel(providers);
    let scheduler_healthy = Arc::new(AtomicBool::new(true));
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let runtime = ChatRuntime {
        inner: Arc::new(RuntimeInner {
            deltas: deltas.clone(),
            providers,
            go_base_url: base_url,
            scheduler_healthy: Arc::clone(&scheduler_healthy),
            default_model,
            max_tokens,
            shutdown: Some(shutdown_sender.clone()),
            scheduler: Mutex::new(None),
            tools: Arc::clone(&tools),
        }),
    };
    let concurrency = concurrency.clamp(1, 64);
    let handle = tokio::spawn(run_scheduler(
        repository,
        provider_receiver,
        deltas,
        concurrency,
        shutdown_receiver,
        shutdown_sender,
        scheduler_healthy,
        tools,
    ));
    if let Ok(mut scheduler) = runtime.inner.scheduler.lock() {
        *scheduler = Some(handle);
    }
    runtime
}

impl ChatRuntime {
    pub fn tools(&self) -> &ToolService {
        &self.inner.tools
    }
    /// Stop new claims and wait for active turns to persist an interrupted or
    /// canceled terminal state before storage shutdown begins.
    pub async fn shutdown(&self) {
        if let Some(shutdown) = &self.inner.shutdown {
            let _ = shutdown.send(true);
        }
        let handle = self
            .inner
            .scheduler
            .lock()
            .ok()
            .and_then(|mut scheduler| scheduler.take());
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }

    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<ChatDelta> {
        self.inner.deltas.subscribe()
    }

    #[must_use]
    pub fn provider_ready(&self) -> bool {
        !self.inner.providers.borrow().is_empty() && self.accepting_work()
    }

    #[must_use]
    pub fn provider_reason(&self) -> Option<&str> {
        if !self.accepting_work() {
            Some("chat runtime is unavailable")
        } else if self.inner.providers.borrow().is_empty() {
            Some("No supported provider credentials are configured")
        } else {
            None
        }
    }

    /// Prepare without changing execution. The daemon persists the key first.
    pub fn prepare_api_key(&self, api_key: &str) -> Result<Arc<OpencodeGoClient>, ProviderError> {
        let client = match self.inner.go_base_url.as_deref() {
            Some(url) => OpencodeGoClient::new_with_base_url(api_key, url)?,
            None => OpencodeGoClient::new(api_key)?,
        };
        Ok(Arc::new(client))
    }

    /// New admissions take this connection; active turns retain their snapshot.
    pub fn replace_provider(&self, provider: Arc<dyn ProviderClient>) {
        let key = provider.descriptor().id().as_str().to_owned();
        self.inner.providers.send_modify(|providers| {
            providers.insert(key, provider);
        });
    }

    #[must_use]
    pub fn provider_client(&self, provider: &str) -> Option<Arc<dyn ProviderClient>> {
        self.inner.providers.borrow().get(provider).cloned()
    }

    #[must_use]
    pub fn accepting_work(&self) -> bool {
        let shutting_down = self
            .inner
            .shutdown
            .as_ref()
            .is_none_or(|sender| *sender.borrow());
        let scheduler_finished =
            self.inner.scheduler.lock().ok().is_none_or(|scheduler| {
                scheduler.as_ref().is_none_or(|handle| handle.is_finished())
            });
        !shutting_down
            && !scheduler_finished
            && self.inner.scheduler_healthy.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn default_model(&self) -> &str {
        &self.inner.default_model
    }

    #[must_use]
    pub fn max_tokens(&self) -> u32 {
        self.inner.max_tokens
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_scheduler<R: ChatRepository>(
    repository: Arc<R>,
    providers: tokio::sync::watch::Receiver<HashMap<String, Arc<dyn ProviderClient>>>,
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    concurrency: usize,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    shutdown_sender: tokio::sync::watch::Sender<bool>,
    scheduler_healthy: Arc<AtomicBool>,
    tools: Arc<ToolService>,
) {
    let mut turns = tokio::task::JoinSet::new();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut claim_failures = 0_u8;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let permit = tokio::select! {
            _ = shutdown.changed() => break,
            joined = turns.join_next(), if !turns.is_empty() => {
                if matches!(joined, Some(Ok(false) | Err(_))) {
                    scheduler_healthy.store(false, Ordering::Release);
                    let _ = shutdown_sender.send(true);
                    break;
                }
                continue;
            },
            permit = Arc::clone(&semaphore).acquire_owned() => match permit { Ok(permit) => permit, Err(_) => return },
        };
        // `claim_next` is a durable mutation. Once submitted, await its result
        // instead of canceling the future: canceling could commit a running
        // claim whose work was never returned to this supervisor.
        let work = match repository.claim_next().await {
            Ok(Some(work)) => {
                claim_failures = 0;
                work
            }
            Ok(None) => {
                claim_failures = 0;
                drop(permit);
                tokio::select! {
                    _ = shutdown.changed() => break,
                    joined = turns.join_next(), if !turns.is_empty() => {
                        if matches!(joined, Some(Ok(false) | Err(_))) {
                            scheduler_healthy.store(false, Ordering::Release);
                            let _ = shutdown_sender.send(true);
                            break;
                        }
                        continue;
                    },
                    _ = tokio::time::sleep(CLAIM_POLL) => {}
                }
                continue;
            }
            Err(_) => {
                claim_failures = claim_failures.saturating_add(1);
                drop(permit);
                if claim_failures >= 50 {
                    scheduler_healthy.store(false, Ordering::Release);
                    let _ = shutdown_sender.send(true);
                    break;
                }
                tokio::select! {
                    _ = shutdown.changed() => break,
                    _ = tokio::time::sleep(CLAIM_POLL) => {}
                }
                continue;
            }
        };
        let mut failed_turn = false;
        while let Some(joined) = turns.try_join_next() {
            if matches!(joined, Ok(false) | Err(_)) {
                failed_turn = true;
            }
        }
        if failed_turn {
            scheduler_healthy.store(false, Ordering::Release);
            let _ = shutdown_sender.send(true);
            let _ =
                finish_with_retry(repository.as_ref(), &work.turn_id, interrupted_shutdown()).await;
            break;
        }
        let repository = Arc::clone(&repository);
        let providers = providers.borrow().clone();
        let deltas = deltas.clone();
        let mut turn_shutdown = shutdown.clone();
        let tools = Arc::clone(&tools);
        turns.spawn(async move {
            let _permit = permit;
            crate::agent::execute(
                repository,
                providers,
                deltas,
                work,
                &mut turn_shutdown,
                tools,
            )
            .await
        });
    }
    while let Some(joined) = turns.join_next().await {
        if matches!(joined, Ok(false) | Err(_)) {
            scheduler_healthy.store(false, Ordering::Release);
            let _ = shutdown_sender.send(true);
        }
    }
}

pub(crate) async fn finish_with_retry<R: ChatRepository>(
    repository: &R,
    turn_id: &str,
    outcome: ChatOutcome,
) -> bool {
    const RETRIES: [Duration; 5] = [
        Duration::ZERO,
        Duration::from_millis(25),
        Duration::from_millis(50),
        Duration::from_millis(100),
        Duration::from_millis(200),
    ];
    for delay in RETRIES {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if repository
            .finish_turn(turn_id, outcome.clone())
            .await
            .is_ok()
        {
            return true;
        }
    }
    false
}

pub(crate) fn failed(code: &str, message: &str) -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Failed,
        error_code: Some(code.to_owned()),
        error_message: Some(message.to_owned()),
    }
}

pub(crate) fn canceled() -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Canceled,
        error_code: None,
        error_message: None,
    }
}

pub(crate) fn interrupted_shutdown() -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Interrupted,
        error_code: Some("daemon_shutdown".to_owned()),
        error_message: Some("Provider work was interrupted by daemon shutdown.".to_owned()),
    }
}

pub(crate) fn provider_failure(error: ProviderError) -> ChatOutcome {
    let (code, message) = match error {
        ProviderError::InvalidModel { .. } => {
            ("unsupported_model", "The requested model is not supported.")
        }
        ProviderError::MissingApiKey { .. } | ProviderError::EmptyApiKey { .. } => (
            "provider_auth_required",
            "OpenCode Go credentials are unavailable.",
        ),
        ProviderError::UnexpectedStatus { status, .. } if status == 401 || status == 403 => (
            "provider_auth_failed",
            "OpenCode Go rejected the configured credentials.",
        ),
        ProviderError::UnsupportedCapability { capability }
            if capability.starts_with("context_incompatible") =>
        {
            (
                "context_incompatible",
                "This context requires continuation from its original model and provider connection.",
            )
        }
        ProviderError::UnsupportedCapability { .. } => (
            "unsupported_capability",
            "The provider does not support the requested capability.",
        ),
        _ => (
            "provider_failed",
            "The provider could not complete this turn.",
        ),
    };
    failed(code, message)
}
