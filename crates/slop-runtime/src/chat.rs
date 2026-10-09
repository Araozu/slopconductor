//! Daemon-owned, bounded text-chat scheduling.
//!
//! The supervisor claims durable turns lazily and runs inference on the shared
//! Tokio runtime. A request connection never owns or waits for execution.

use std::{
    error::Error,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::providers::opencode_go::OpencodeGoClient;
use crate::providers::{
    ChatMessage, ChatRequest, ProviderClient, ProviderError, Role, StreamDelta, TurnOutcome, Usage,
};
use slop_core::provider::ProviderModelRef;

pub const DEFAULT_MODEL: &str = "opencode-go/glm-5.3-flash";
pub const DEFAULT_OUTPUT_TOKENS: u32 = 4_096;
pub const DEFAULT_CONCURRENCY: usize = 4;
const CHECKPOINT_INTERVAL: Duration = Duration::from_millis(250);
const CHECKPOINT_BYTES: usize = 16 * 1024;
const CANCELLATION_POLL: Duration = Duration::from_millis(250);
const CLAIM_POLL: Duration = Duration::from_millis(100);
const MAX_VISIBLE_BYTES: usize = 1024 * 1024;

pub type RepoError = Box<dyn Error + Send + Sync>;
pub type RepoFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RepoError>> + Send + 'a>>;

/// Storage boundary for accepted chat turns. Implementations own transaction
/// semantics and wire-independent persistence; the runtime never sees SQL/DTOs.
pub trait ChatRepository: Send + Sync + 'static {
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnWork {
    pub turn_id: String,
    pub session_id: String,
    pub requested_model: String,
    pub max_tokens: Option<u32>,
    pub messages: Vec<ContextMessage>,
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
}

#[derive(Clone)]
pub struct ChatRuntime {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    provider: tokio::sync::watch::Sender<Option<Arc<OpencodeGoClient>>>,
    provider_base_url: Option<String>,
    scheduler_healthy: Arc<AtomicBool>,
    default_model: String,
    max_tokens: u32,
    shutdown: Option<tokio::sync::watch::Sender<bool>>,
    scheduler: Mutex<Option<tokio::task::JoinHandle<()>>>,
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
    let (deltas, _) = tokio::sync::broadcast::channel(128);
    let default_model = default_model.unwrap_or_else(|| DEFAULT_MODEL.to_owned());
    let max_tokens = max_tokens.unwrap_or(DEFAULT_OUTPUT_TOKENS);
    let client = api_key.as_deref().and_then(|key| {
        let result = match base_url.as_deref() {
            Some(url) => OpencodeGoClient::new_with_base_url(key, url),
            None => OpencodeGoClient::new(key),
        };
        result.ok()
    });
    let (provider, provider_receiver) = tokio::sync::watch::channel(client.map(Arc::new));
    let scheduler_healthy = Arc::new(AtomicBool::new(true));
    let (shutdown_sender, shutdown_receiver) = tokio::sync::watch::channel(false);
    let runtime = ChatRuntime {
        inner: Arc::new(RuntimeInner {
            deltas: deltas.clone(),
            provider,
            provider_base_url: base_url,
            scheduler_healthy: Arc::clone(&scheduler_healthy),
            default_model,
            max_tokens,
            shutdown: Some(shutdown_sender.clone()),
            scheduler: Mutex::new(None),
        }),
    };
    let concurrency = concurrency.clamp(1, 64);
    let handle = tokio::spawn(run_scheduler(
        repository,
        provider_receiver,
        deltas,
        concurrency,
        max_tokens,
        shutdown_receiver,
        shutdown_sender,
        scheduler_healthy,
    ));
    if let Ok(mut scheduler) = runtime.inner.scheduler.lock() {
        *scheduler = Some(handle);
    }
    runtime
}

impl ChatRuntime {
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
        self.inner.provider.borrow().is_some() && self.accepting_work()
    }

    #[must_use]
    pub fn provider_reason(&self) -> Option<&str> {
        if !self.accepting_work() {
            Some("chat runtime is unavailable")
        } else if self.inner.provider.borrow().is_none() {
            Some("OpenCode Go credentials are unavailable; configure a provider API key")
        } else {
            None
        }
    }

    /// Prepare without changing execution. The daemon persists the key first.
    pub fn prepare_api_key(&self, api_key: &str) -> Result<Arc<OpencodeGoClient>, ProviderError> {
        let client = match self.inner.provider_base_url.as_deref() {
            Some(url) => OpencodeGoClient::new_with_base_url(api_key, url)?,
            None => OpencodeGoClient::new(api_key)?,
        };
        Ok(Arc::new(client))
    }

    /// New admissions take this connection; active turns retain their snapshot.
    pub fn replace_provider(&self, provider: Arc<OpencodeGoClient>) {
        self.inner.provider.send_replace(Some(provider));
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

struct VisibleBuffer {
    text: String,
    last_checkpoint_bytes: usize,
    last_checkpoint_at: tokio::time::Instant,
}

#[allow(clippy::too_many_arguments)]
async fn run_scheduler<R: ChatRepository>(
    repository: Arc<R>,
    provider: tokio::sync::watch::Receiver<Option<Arc<OpencodeGoClient>>>,
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    concurrency: usize,
    default_max_tokens: u32,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    shutdown_sender: tokio::sync::watch::Sender<bool>,
    scheduler_healthy: Arc<AtomicBool>,
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
        let provider = provider.borrow().clone();
        let deltas = deltas.clone();
        let mut turn_shutdown = shutdown.clone();
        turns.spawn(async move {
            let _permit = permit;
            execute_turn(
                repository,
                provider,
                deltas,
                work,
                default_max_tokens,
                &mut turn_shutdown,
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

async fn execute_turn<R: ChatRepository>(
    repository: Arc<R>,
    provider: Option<Arc<OpencodeGoClient>>,
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    work: TurnWork,
    default_max_tokens: u32,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    if *shutdown.borrow() {
        return finish_with_retry(repository.as_ref(), &work.turn_id, interrupted_shutdown()).await;
    }
    match repository.cancellation_requested(&work.turn_id).await {
        Ok(true) => {
            return finish_with_retry(repository.as_ref(), &work.turn_id, canceled()).await;
        }
        Ok(false) => {}
        Err(_) => {
            return finish_with_retry(
                repository.as_ref(),
                &work.turn_id,
                failed("storage_unavailable", "The turn state could not be read."),
            )
            .await;
        }
    }
    if work.messages.len() > ChatRequest::MAX_MESSAGES
        || work.messages.iter().map(|m| m.text.len()).sum::<usize>()
            > ChatRequest::MAX_REQUEST_BYTES
    {
        return finish_with_retry(
            repository.as_ref(),
            &work.turn_id,
            failed("context_limit", "Turn context exceeds the supported bound."),
        )
        .await;
    }
    let model_ref = match work.requested_model.parse::<ProviderModelRef>() {
        Ok(model_ref) if model_ref.provider().as_str() == "opencode-go" => model_ref,
        _ => {
            return finish_with_retry(
                repository.as_ref(),
                &work.turn_id,
                failed(
                    "unsupported_model",
                    "The requested provider or model is not supported.",
                ),
            )
            .await;
        }
    };
    let request = ChatRequest {
        model: model_ref.model().to_owned(),
        messages: work
            .messages
            .iter()
            .map(|message| ChatMessage {
                role: match message.role {
                    RoleKind::System => Role::System,
                    RoleKind::Developer => Role::Developer,
                    RoleKind::User => Role::User,
                    RoleKind::Assistant => Role::Assistant,
                },
                content: message.text.clone(),
            })
            .collect(),
        max_tokens: Some(work.max_tokens.unwrap_or(default_max_tokens)),
        session_id: work.session_id.clone(),
    };
    let Some(provider) = provider else {
        return finish_with_retry(
            repository.as_ref(),
            &work.turn_id,
            failed(
                "provider_auth_required",
                "OpenCode Go credentials are unavailable.",
            ),
        )
        .await;
    };
    if let Err(error) = provider.validate(&request) {
        return finish_with_retry(repository.as_ref(), &work.turn_id, provider_failure(error))
            .await;
    }

    let visible = Arc::new(Mutex::new(VisibleBuffer {
        text: String::new(),
        last_checkpoint_bytes: 0,
        last_checkpoint_at: tokio::time::Instant::now(),
    }));
    let stream_visible = Arc::clone(&visible);
    let stream_deltas = deltas.clone();
    let session_id = work.session_id.clone();
    let turn_id = work.turn_id.clone();
    let mut inference = Box::pin(provider.complete_streaming(
        &request,
        move |delta: StreamDelta| {
            if delta.text.is_empty() {
                return;
            }
            let mut offset = 0;
            while offset < delta.text.len() {
                let mut end = (offset + 16 * 1024).min(delta.text.len());
                while !delta.text.is_char_boundary(end) {
                    end -= 1;
                }
                let chunk = &delta.text[offset..end];
                if let Ok(mut visible) = stream_visible.lock()
                    && visible.text.len().saturating_add(chunk.len()) <= MAX_VISIBLE_BYTES
                {
                    visible.text.push_str(chunk);
                    let _ = stream_deltas.send(ChatDelta {
                        session_id: session_id.clone(),
                        turn_id: turn_id.clone(),
                        text: chunk.to_owned(),
                    });
                }
                offset = end;
            }
        },
    ));
    let mut checkpoint_tick = tokio::time::interval(CHECKPOINT_INTERVAL);
    let mut cancel_tick = tokio::time::interval(CANCELLATION_POLL);
    checkpoint_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Skip the immediate first ticks; the first checkpoint is after visible
    // output, while cancellation is checked promptly after request start.
    checkpoint_tick.tick().await;
    let mut storage_failed = false;
    let inference_result = loop {
        tokio::select! {
            result = &mut inference => break Some(result),
            _ = shutdown.changed() => break None,
            _ = cancel_tick.tick() => {
                match repository.cancellation_requested(&work.turn_id).await {
                    Ok(true) => break None,
                    Ok(false) => {}
                    Err(_) => { storage_failed = true; break None; }
                }
            }
            _ = checkpoint_tick.tick() => {
                if checkpoint_if_due(repository.as_ref(), &work.turn_id, &visible, false).await.is_err() {
                    storage_failed = true;
                    break None;
                }
            }
        }
    };
    drop(inference);
    if storage_failed {
        return finish_with_retry(
            repository.as_ref(),
            &work.turn_id,
            failed(
                "storage_unavailable",
                "The durable checkpoint could not be committed.",
            ),
        )
        .await;
    }
    match inference_result {
        Some(Ok(response)) => {
            let (status, error_code, error_message) = match response.outcome {
                TurnOutcome::Completed => (ChatStatus::Completed, None, None),
                TurnOutcome::Incomplete { reason } => (
                    ChatStatus::Incomplete,
                    Some("provider_incomplete".to_owned()),
                    Some(bound_reason(reason)),
                ),
            };
            let outcome = ChatOutcome {
                text: response.text,
                resolved_model: response.resolved_model,
                usage: response.usage,
                status,
                error_code,
                error_message,
            };
            return finish_with_retry(repository.as_ref(), &work.turn_id, outcome).await;
        }
        Some(Err(error)) => {
            // Incomplete/interrupted provider text is not promoted to a final
            // assistant message. The durable checkpoint remains explicitly
            // incomplete and is excluded by storage from later context.
            return finish_with_retry(repository.as_ref(), &work.turn_id, provider_failure(error))
                .await;
        }
        None => {
            let cancelled = match repository.cancellation_requested(&work.turn_id).await {
                Ok(cancelled) => cancelled,
                Err(_) => {
                    return finish_with_retry(
                        repository.as_ref(),
                        &work.turn_id,
                        failed("storage_unavailable", "The turn state could not be read."),
                    )
                    .await;
                }
            };
            let outcome = if cancelled {
                canceled()
            } else {
                interrupted_shutdown()
            };
            return finish_with_retry(repository.as_ref(), &work.turn_id, outcome).await;
        }
    }
}

async fn checkpoint_if_due<R: ChatRepository>(
    repository: &R,
    turn_id: &str,
    visible: &Mutex<VisibleBuffer>,
    force: bool,
) -> Result<(), RepoError> {
    let snapshot = {
        let mut buffer = visible
            .lock()
            .map_err(|_| std::io::Error::other("visible buffer poisoned"))?;
        let advanced = buffer
            .text
            .len()
            .saturating_sub(buffer.last_checkpoint_bytes);
        if !force
            && advanced < CHECKPOINT_BYTES
            && buffer.last_checkpoint_at.elapsed() < CHECKPOINT_INTERVAL
        {
            return Ok(());
        }
        if advanced == 0 && !force {
            return Ok(());
        }
        buffer.last_checkpoint_bytes = buffer.text.len();
        buffer.last_checkpoint_at = tokio::time::Instant::now();
        buffer.text.clone()
    };
    repository.checkpoint_visible(turn_id, &snapshot).await
}

async fn finish_with_retry<R: ChatRepository>(
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

fn failed(code: &str, message: &str) -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Failed,
        error_code: Some(code.to_owned()),
        error_message: Some(message.to_owned()),
    }
}

fn canceled() -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Canceled,
        error_code: None,
        error_message: None,
    }
}

fn interrupted_shutdown() -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage: Usage::default(),
        status: ChatStatus::Interrupted,
        error_code: Some("daemon_shutdown".to_owned()),
        error_message: Some("Provider work was interrupted by daemon shutdown.".to_owned()),
    }
}

fn provider_failure(error: ProviderError) -> ChatOutcome {
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
        _ => (
            "provider_failed",
            "The provider could not complete this turn.",
        ),
    };
    failed(code, message)
}

fn bound_reason(reason: String) -> String {
    reason
        .chars()
        .filter(|c| !c.is_control())
        .take(256)
        .collect()
}
