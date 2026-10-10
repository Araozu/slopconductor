//! Daemon-owned SQLite store for node identity and durable text chat.

use std::{
    env,
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use slop_protocol::{
    NodeResponse,
    chat::{
        CancelTurnRequest, CommandReceipt, CreateSessionRequest, DeliveryMode, EventResponse,
        MessageResponse, Page, SendMessageRequest, SessionResponse, SteeringInstructionResponse,
        TurnControlRequest, TurnResponse, UsageResponse,
    },
};
use tokio::sync::oneshot;

mod execution;
pub(crate) mod orchestration;
mod projects;

const DATABASE_FILE: &str = "state.sqlite3";
const LOCK_FILE: &str = "daemon.lock";
const SCHEMA_VERSION: i64 = 6;
const MINIMUM_SQLITE_VERSION: i32 = 3_051_003;
const MAX_QUEUE_CAPACITY: usize = 4096;
const MAX_BUSY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_NODE_NAME_BYTES: usize = 128;
const MAX_COMMAND_ID_BYTES: usize = 128;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
const MAX_CONTEXT_MESSAGES: usize = 256;
const MAX_MESSAGE_PAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_QUEUE_PER_SESSION: i64 = 32;
const MAX_QUEUE_GLOBAL: i64 = 1024;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 200;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Errors surfaced by the bounded storage service.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database queue is full")]
    Busy,
    #[error("the data directory is already owned by another daemon")]
    AlreadyRunning,
    #[error("storage service is unavailable")]
    Unavailable,
    #[error("invalid storage configuration")]
    Configuration,
    #[error("database schema version {found} is newer than supported version {supported}")]
    NewerSchema { found: i64, supported: i64 },
    #[error("database operation failed")]
    Database,
    #[error("storage worker failed")]
    Worker,
    #[error("storage worker could not close cleanly")]
    Close,
    #[error("invalid chat command")]
    Invalid,
    #[error("chat record was not found")]
    NotFound,
    #[error("chat command conflicts with existing state")]
    Conflict,
    #[error("chat storage limit reached")]
    Limit,
    #[error("unsupported execution capability: {0}")]
    Unsupported(&'static str),
    #[error("active-turn steering cannot change its frozen model or settings")]
    ActiveTurnOverride,
    #[error("message preflight failed: {0}")]
    Preflight(&'static str, &'static str),
    #[error("Git operation failed: {0}")]
    Git(#[from] slop_runtime::git::GitError),
}

type StoreResult<T> = Result<T, StoreError>;

struct Gate {
    accepting: bool,
}

struct Shared {
    sender: SyncSender<Request>,
    gate: Arc<Mutex<Gate>>,
    alive: Arc<AtomicBool>,
    data_dir: PathBuf,
    git: slop_runtime::git::GitService,
    workspace_jobs: Arc<tokio::sync::Mutex<tokio::task::JoinSet<()>>>,
    workspace_healthy: AtomicBool,
}

enum Request {
    Node(oneshot::Sender<StoreResult<NodeResponse>>),
    Job(Box<dyn FnOnce(&Connection) + Send>),
    #[cfg(test)]
    Hold {
        started: oneshot::Sender<()>,
        release: oneshot::Receiver<()>,
    },
}

struct WorkerArgs {
    data_dir: PathBuf,
    lock: File,
    receiver: Receiver<Request>,
    node_name: Option<String>,
    busy_timeout: Duration,
    gate: Arc<Mutex<Gate>>,
    ready: oneshot::Sender<StoreResult<()>>,
    finished: oneshot::Sender<StoreResult<()>>,
}

/// A handle to the single daemon-wide database worker.
pub struct Store {
    shared: Arc<Shared>,
    finished: Option<oneshot::Receiver<StoreResult<()>>>,
    worker: Option<JoinHandle<()>>,
}

/// A cloneable bounded client for daemon storage queries.
#[derive(Clone)]
pub struct StoreClient {
    shared: Arc<Shared>,
}

struct StartupGuard(Option<(Arc<AtomicBool>, Arc<Mutex<Gate>>)>);

impl Drop for StartupGuard {
    fn drop(&mut self) {
        if let Some((alive, gate)) = self.0.take() {
            if let Ok(mut gate) = gate.lock() {
                gate.accepting = false;
            }
            alive.store(false, Ordering::Release);
        }
    }
}

impl Store {
    /// Create or open the private data directory and start its sole DB worker.
    pub async fn open(
        data_dir: PathBuf,
        node_name: Option<String>,
        queue_capacity: usize,
        busy_timeout: Duration,
    ) -> StoreResult<Self> {
        if queue_capacity == 0
            || queue_capacity > MAX_QUEUE_CAPACITY
            || busy_timeout < Duration::from_millis(1)
            || busy_timeout > MAX_BUSY_TIMEOUT
            || node_name
                .as_ref()
                .is_some_and(|name| !valid_node_name(name))
        {
            return Err(StoreError::Configuration);
        }

        prepare_data_dir(&data_dir)?;
        let canonical_dir = fs::canonicalize(&data_dir).map_err(|_| StoreError::Configuration)?;
        verify_private_data_dir(&canonical_dir)?;

        let lock = open_lock(&canonical_dir)?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => StoreError::AlreadyRunning,
            std::fs::TryLockError::Error(_) => StoreError::Configuration,
        })?;

        let (sender, receiver) = mpsc::sync_channel(queue_capacity);
        let alive = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(Mutex::new(Gate { accepting: true }));
        let shared = Arc::new(Shared {
            sender,
            gate: Arc::clone(&gate),
            alive: Arc::clone(&alive),
            data_dir: canonical_dir.clone(),
            git: slop_runtime::git::GitService::new(canonical_dir.clone()),
            workspace_jobs: Arc::new(tokio::sync::Mutex::new(tokio::task::JoinSet::new())),
            workspace_healthy: AtomicBool::new(true),
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        let (finished_tx, finished_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("slopconductor-db".to_owned())
            .spawn(move || {
                worker_main(WorkerArgs {
                    data_dir: canonical_dir,
                    lock,
                    receiver,
                    node_name,
                    busy_timeout,
                    gate,
                    ready: ready_tx,
                    finished: finished_tx,
                });
            })
            .map_err(|_| StoreError::Unavailable)?;

        let mut startup_guard =
            StartupGuard(Some((Arc::clone(&shared.alive), Arc::clone(&shared.gate))));
        match ready_rx.await {
            Ok(Ok(())) => {
                startup_guard.0.take();
                Ok(Self {
                    shared,
                    finished: Some(finished_rx),
                    worker: Some(worker),
                })
            }
            Ok(Err(error)) => {
                stop_accepting(&shared);
                let _ = tokio::task::spawn_blocking(move || worker.join()).await;
                Err(error)
            }
            Err(_) => {
                stop_accepting(&shared);
                let _ = tokio::task::spawn_blocking(move || worker.join()).await;
                Err(StoreError::Unavailable)
            }
        }
    }

    /// Get a cloneable handle that submits work to this store's bounded queue.
    pub fn client(&self) -> StoreClient {
        StoreClient {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Stop accepting work, drain accepted requests, close SQLite, then join.
    pub async fn shutdown(mut self) -> StoreResult<()> {
        stop_accepting(&self.shared);
        let finished = self.finished.take().ok_or(StoreError::Unavailable)?;
        let result = finished.await.map_err(|_| StoreError::Unavailable)?;
        let worker = self.worker.take().ok_or(StoreError::Unavailable)?;
        let joined = tokio::task::spawn_blocking(move || worker.join())
            .await
            .map_err(|_| StoreError::Worker)?;
        if joined.is_err() {
            return Err(StoreError::Worker);
        }
        result
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        stop_accepting(&self.shared);
    }
}

impl StoreClient {
    /// Inspect at most the bounded chat context needed for provider preflight.
    /// Private continuation items remain inside the storage/runtime boundary.
    pub async fn context_requirements(
        &self,
        session_id: &str,
    ) -> StoreResult<(bool, Vec<(String, String, String)>)> {
        let session_id = session_id.to_owned();
        self.submit(move |connection| {
            let mut statement = connection.prepare(
                "SELECT m.blocks,m.continuation FROM messages m JOIN turns t ON t.id=m.turn_id
                 WHERE m.session_id=?1 AND m.status='completed'
                   AND (t.status='completed'
                     OR (t.status IN('failed','cancelled','interrupted','incomplete') AND EXISTS(SELECT 1 FROM tool_invocations i WHERE i.turn_id=t.id))
                     OR (t.status='running' AND EXISTS(SELECT 1 FROM tool_invocations i WHERE i.turn_id=t.id)))
                 ORDER BY m.ordinal LIMIT 256",
            ).map_err(|_| StoreError::Database)?;
            let mut rows = statement.query([session_id]).map_err(|_| StoreError::Database)?;
            let mut non_text = false;
            let mut required = Vec::new();
            while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
                let blocks: Vec<slop_runtime::providers::inference::ContentBlock> = execution::json_column(row, 0).map_err(|_| StoreError::Database)?;
                non_text |= blocks.iter().any(|block| !matches!(block.content, slop_runtime::providers::inference::BlockContent::Text { .. }));
                let continuation: Option<slop_runtime::providers::inference::Continuation> = execution::optional_json_column(row, 1).map_err(|_| StoreError::Database)?;
                if let Some(continuation) = continuation.filter(|value| value.required) {
                    required.push((continuation.provider, continuation.model, continuation.wire));
                }
            }
            Ok((non_text, required))
        }).await
    }

    pub async fn command_seen(&self, command_id: &str) -> StoreResult<bool> {
        let command_id = command_id.to_owned();
        self.submit(move |connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM commands WHERE command_id=?1)",
                    [command_id],
                    |row| row.get(0),
                )
                .map_err(|_| StoreError::Database)
        })
        .await
    }

    /// Query the persisted identity through the database worker.
    pub async fn node(&self) -> StoreResult<NodeResponse> {
        let (response_tx, response_rx) = oneshot::channel();
        {
            let gate = self
                .shared
                .gate
                .lock()
                .map_err(|_| StoreError::Unavailable)?;
            if !gate.accepting || !self.shared.alive.load(Ordering::Acquire) {
                return Err(StoreError::Unavailable);
            }
            match self.shared.sender.try_send(Request::Node(response_tx)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => return Err(StoreError::Busy),
                Err(TrySendError::Disconnected(_)) => return Err(StoreError::Unavailable),
            }
        }
        response_rx.await.map_err(|_| StoreError::Unavailable)?
    }

    /// Create a durable session, or return the original receipt for a retry.
    #[allow(dead_code)] // Also useful to embedded daemon users without custom defaults.
    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> StoreResult<CommandReceipt> {
        self.create_session_with_default_max_tokens(request, Some(4096))
            .await
    }

    /// Persist the configured effective token cap while retaining the
    /// caller's original request as the idempotency payload.
    pub async fn create_session_with_default_max_tokens(
        &self,
        request: CreateSessionRequest,
        default_max_tokens: Option<u32>,
    ) -> StoreResult<CommandReceipt> {
        if !self.shared.workspace_healthy.load(Ordering::Acquire) {
            return Err(StoreError::Unavailable);
        }
        let resolved = self.resolve_project_workspace(&request).await?;
        self.submit(move |connection| {
            create_session(connection, request, default_max_tokens, resolved)
        })
        .await
    }

    pub async fn sessions(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<SessionResponse>> {
        let limit = page_limit(limit)?;
        self.submit(move |connection| list_sessions(connection, after, limit))
            .await
    }

    pub async fn session(&self, id: &str) -> StoreResult<SessionResponse> {
        let id = id.to_owned();
        self.submit(move |connection| get_session(connection, &id))
            .await
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        request: SendMessageRequest,
    ) -> StoreResult<CommandReceipt> {
        self.send_message_with_effective_settings(session_id, request, None)
            .await
    }

    pub async fn send_message_with_effective_settings(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    ) -> StoreResult<CommandReceipt> {
        let session_id = session_id.to_owned();
        self.submit(move |connection| {
            send_message(connection, &session_id, request, effective_settings, None)
        })
        .await
    }

    pub async fn send_message_preflighted(
        &self,
        session_id: &str,
        request: SendMessageRequest,
        effective_settings: slop_protocol::execution::GenerationSettings,
        idle_failure: Option<(&'static str, &'static str)>,
    ) -> StoreResult<CommandReceipt> {
        if !self.shared.workspace_healthy.load(Ordering::Acquire) {
            return Err(StoreError::Unavailable);
        }
        let session_id = session_id.to_owned();
        self.submit(move |connection| {
            send_message(
                connection,
                &session_id,
                request,
                Some(effective_settings),
                idle_failure,
            )
        })
        .await
    }

    pub async fn messages(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<MessageResponse>> {
        let session_id = session_id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |connection| list_messages(connection, &session_id, after, limit))
            .await
    }

    pub async fn events(
        &self,
        session_id: &str,
        after: u64,
        limit: usize,
    ) -> StoreResult<Page<EventResponse>> {
        let session_id = session_id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |connection| list_events(connection, &session_id, after, limit))
            .await
    }

    pub async fn turn(&self, id: &str) -> StoreResult<TurnResponse> {
        let id = id.to_owned();
        self.submit(move |connection| get_turn(connection, &id))
            .await
    }

    pub async fn cancel_turn(
        &self,
        turn_id: &str,
        request: CancelTurnRequest,
    ) -> StoreResult<CommandReceipt> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| cancel_turn(connection, &turn_id, request))
            .await
    }

    pub async fn pause_turn(
        &self,
        turn_id: &str,
        request: TurnControlRequest,
    ) -> StoreResult<CommandReceipt> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| turn_control(connection, &turn_id, request, "pause"))
            .await
    }

    pub async fn resume_turn(
        &self,
        turn_id: &str,
        request: TurnControlRequest,
    ) -> StoreResult<CommandReceipt> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| turn_control(connection, &turn_id, request, "resume"))
            .await
    }

    pub async fn steering_instructions(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<SteeringInstructionResponse>> {
        let session_id = session_id.to_owned();
        let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
        let limit = page_limit(limit)?;
        self.submit(move |connection| list_steering(connection, &session_id, after, limit))
            .await
    }

    pub async fn claim_next_turn(&self) -> StoreResult<Option<slop_runtime::chat::TurnWork>> {
        self.submit(claim_next_turn).await
    }

    pub async fn checkpoint_visible(&self, turn_id: &str, text: &str) -> StoreResult<()> {
        let turn_id = turn_id.to_owned();
        let text = text.to_owned();
        self.submit(move |connection| checkpoint_turn(connection, &turn_id, &text))
            .await
    }

    pub async fn finish_chat_turn(
        &self,
        turn_id: &str,
        outcome: slop_runtime::chat::ChatOutcome,
    ) -> StoreResult<()> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| finish_turn(connection, &turn_id, outcome))
            .await
    }

    pub async fn turn_cancellation_requested(&self, turn_id: &str) -> StoreResult<bool> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| cancellation_requested(connection, &turn_id))
            .await
    }

    pub async fn turn_control(
        &self,
        turn_id: &str,
    ) -> StoreResult<slop_runtime::chat::TurnControl> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| turn_control_state(connection, &turn_id))
            .await
    }

    pub async fn retain_provider_snapshot(&self, turn_id: &str) -> StoreResult<bool> {
        let turn_id = turn_id.to_owned();
        self.submit(move |connection| {
            let queued: bool = connection.query_row(
                "SELECT status IN ('queued','running') AND EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='applied') FROM turns WHERE id=?1",
                [&turn_id],
                |row| row.get(0),
            ).optional().map_err(|_| StoreError::Database)?.ok_or(StoreError::NotFound)?;
            Ok(queued)
        }).await
    }

    async fn submit<T, F>(&self, operation: F) -> StoreResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> StoreResult<T> + Send + 'static,
    {
        let (response_tx, response_rx) = oneshot::channel();
        {
            let gate = self
                .shared
                .gate
                .lock()
                .map_err(|_| StoreError::Unavailable)?;
            if !gate.accepting || !self.shared.alive.load(Ordering::Acquire) {
                return Err(StoreError::Unavailable);
            }
            let job = Box::new(move |connection: &Connection| {
                let _ = response_tx.send(operation(connection));
            });
            match self.shared.sender.try_send(Request::Job(job)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => return Err(StoreError::Busy),
                Err(TrySendError::Disconnected(_)) => return Err(StoreError::Unavailable),
            }
        }
        response_rx.await.map_err(|_| StoreError::Unavailable)?
    }
}

fn stop_accepting(shared: &Shared) {
    if let Ok(mut gate) = shared.gate.lock() {
        gate.accepting = false;
    }
    shared.alive.store(false, Ordering::Release);
}

fn worker_main(args: WorkerArgs) {
    let WorkerArgs {
        data_dir,
        lock,
        receiver,
        node_name,
        busy_timeout,
        gate,
        ready,
        finished,
    } = args;
    let connection = match open_and_initialize(&data_dir, node_name, busy_timeout) {
        Ok(connection) => {
            let _ = ready.send(Ok(()));
            connection
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            drop(lock);
            let _ = finished.send(Ok(()));
            return;
        }
    };

    loop {
        match receiver.recv_timeout(WORKER_POLL_INTERVAL) {
            Ok(Request::Node(reply)) => {
                let _ = reply.send(query_node(&connection));
            }
            Ok(Request::Job(job)) => job(&connection),
            #[cfg(test)]
            Ok(Request::Hold { started, release }) => {
                let _ = started.send(());
                let _ = release.blocking_recv();
            }
            Err(RecvTimeoutError::Timeout)
                if gate.lock().map(|state| state.accepting).unwrap_or(false) =>
            {
                continue;
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    let close_result = connection.close().map_err(|_| StoreError::Close);
    // The ownership lock remains held until the SQLite connection has closed.
    drop(lock);
    let _ = finished.send(close_result);
}

fn prepare_data_dir(path: &Path) -> StoreResult<()> {
    if path.as_os_str().is_empty() {
        return Err(StoreError::Configuration);
    }
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(path)
                .map_err(|_| StoreError::Configuration)?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path).map_err(|_| StoreError::Configuration)?;
    }
    Ok(())
}

fn verify_private_data_dir(path: &Path) -> StoreResult<()> {
    let metadata = fs::metadata(path).map_err(|_| StoreError::Configuration)?;
    if !metadata.is_dir() {
        return Err(StoreError::Configuration);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let effective_uid = rustix::process::geteuid().as_raw();
        if metadata.uid() != effective_uid || metadata.mode() & 0o077 != 0 {
            return Err(StoreError::Configuration);
        }
    }
    Ok(())
}

fn open_lock(data_dir: &Path) -> StoreResult<File> {
    let path = data_dir.join(LOCK_FILE);
    verify_regular_or_absent(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|_| StoreError::Configuration)?;
    if !file
        .metadata()
        .map_err(|_| StoreError::Configuration)?
        .is_file()
    {
        return Err(StoreError::Configuration);
    }
    Ok(file)
}

fn open_and_initialize(
    data_dir: &Path,
    node_name: Option<String>,
    busy_timeout: Duration,
) -> StoreResult<Connection> {
    let db_path = data_dir.join(DATABASE_FILE);
    verify_regular_or_absent(&db_path)?;
    verify_regular_or_absent(&data_dir.join(format!("{DATABASE_FILE}-wal")))?;
    verify_regular_or_absent(&data_dir.join(format!("{DATABASE_FILE}-shm")))?;
    let existed = db_path.exists();
    let connection = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|_| StoreError::Database)?;
    connection
        .busy_timeout(busy_timeout)
        .map_err(|_| StoreError::Database)?;

    // Read the schema version before any setting that can modify a newer DB.
    let existing_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    if existing_version > SCHEMA_VERSION {
        return Err(StoreError::NewerSchema {
            found: existing_version,
            supported: SCHEMA_VERSION,
        });
    }
    if existing_version < 0 {
        return Err(StoreError::Database);
    }

    if existed && existing_version == 0 {
        let has_objects: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type IN ('table','index','trigger','view') AND name NOT LIKE 'sqlite_%')",
                [],
                |row| row.get(0),
            )
            .map_err(|_| StoreError::Database)?;
        if has_objects {
            return Err(StoreError::Database);
        }
    }

    // Refuse SQLite builds with the WAL-reset corruption bug before changing
    // journal mode or schema. The bundled 3.53.2 build exceeds this floor.
    if rusqlite::version_number() < MINIMUM_SQLITE_VERSION {
        return Err(StoreError::Configuration);
    }

    let mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    if !mode.eq_ignore_ascii_case("wal") {
        let changed: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|_| StoreError::Database)?;
        if !changed.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Database);
        }
    }
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|_| StoreError::Database)?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|_| StoreError::Database)?;

    migrate(&connection, existing_version, node_name)?;
    recover_interrupted(&connection)?;
    query_node(&connection)?;
    verify_pragmas(&connection, busy_timeout)?;
    Ok(connection)
}

fn verify_regular_or_absent(path: &Path) -> StoreResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(StoreError::Configuration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StoreError::Configuration),
    }
}

fn migrate(
    connection: &Connection,
    existing_version: i64,
    node_name: Option<String>,
) -> StoreResult<()> {
    migrate_with_hook(connection, existing_version, node_name, |_| Ok(()))
}

fn migrate_with_hook<F>(
    connection: &Connection,
    existing_version: i64,
    node_name: Option<String>,
    hook: F,
) -> StoreResult<()>
where
    F: FnOnce(&Transaction<'_>) -> StoreResult<()>,
{
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    if existing_version == 0 {
        tx.execute_batch(
            "CREATE TABLE node_identity (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                node_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                os TEXT NOT NULL
            );",
        )
        .map_err(|_| StoreError::Database)?;
        let identity = node_name.unwrap_or_else(default_node_name);
        tx.execute(
            "INSERT INTO node_identity (singleton, node_id, name, os) VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![random_node_id()?, identity, env::consts::OS],
        )
        .map_err(|_| StoreError::Database)?;
    } else if let Some(name) = node_name {
        let existing = query_node(&tx)?;
        validate_identity(&existing)?;
        tx.execute(
            "UPDATE node_identity SET name = ?1 WHERE singleton = 1",
            [name],
        )
        .map_err(|_| StoreError::Database)?;
        if tx.changes() != 1 {
            return Err(StoreError::Database);
        }
    }
    if existing_version <= 1 {
        create_chat_schema(&tx)?;
    }
    if existing_version < 3 {
        execution::migrate(&tx)?;
    }
    if existing_version < 4 {
        steering_migrate(&tx)?;
    }
    if existing_version < 5 {
        projects::migrate(&tx)?;
    }
    if existing_version < 6 {
        orchestration::migrate(&tx)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|_| StoreError::Database)?;
    hook(&tx)?;
    tx.commit().map_err(|_| StoreError::Database)
}

fn create_chat_schema(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            owner_node_id TEXT NOT NULL,
            title TEXT,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            max_tokens INTEGER,
            revision INTEGER NOT NULL DEFAULT 0,
            last_event_sequence INTEGER NOT NULL DEFAULT 0,
            created_order INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE turns (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            user_message_id TEXT NOT NULL,
            assistant_message_id TEXT,
            status TEXT NOT NULL,
            requested_model TEXT NOT NULL,
            resolved_model TEXT,
            input_tokens TEXT,
            output_tokens TEXT,
            total_tokens TEXT,
            total_source TEXT,
            error_code TEXT,
            error_message TEXT,
            cancellation_requested INTEGER NOT NULL DEFAULT 0,
            UNIQUE(session_id, ordinal)
        );
        CREATE INDEX turns_status ON turns(status);
        CREATE TABLE messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            status TEXT NOT NULL,
            UNIQUE(session_id, ordinal)
        );
        CREATE INDEX messages_session_order ON messages(session_id, ordinal);
        CREATE TABLE session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            kind TEXT NOT NULL,
            turn_id TEXT,
            message_id TEXT,
            revision INTEGER NOT NULL,
            PRIMARY KEY(session_id, sequence)
        );
        CREATE TABLE commands (
            command_id TEXT PRIMARY KEY,
            scope TEXT NOT NULL,
            payload TEXT NOT NULL,
            receipt TEXT NOT NULL
        );
        CREATE TABLE session_order (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            next_order INTEGER NOT NULL
        );
        INSERT INTO session_order(singleton,next_order) VALUES(1,1);",
    )
    .map_err(|_| StoreError::Database)
}

fn steering_migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE turns ADD COLUMN immediate_requested INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN pause_requested INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN usage_unknown INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;
         CREATE TABLE steering_instructions (
            id TEXT PRIMARY KEY,
            command_id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            text TEXT NOT NULL,
            delivery TEXT NOT NULL CHECK(delivery IN ('next-boundary','immediate')),
            status TEXT NOT NULL CHECK(status IN ('accepted','applied','rejected')),
            error_code TEXT,
            created_order INTEGER NOT NULL UNIQUE
         );
         CREATE INDEX steering_turn_status ON steering_instructions(turn_id,status,created_order);",
    )
    .map_err(|_| StoreError::Database)
}

const MAX_PENDING_STEERING_PER_TURN: i64 = 32;

fn default_node_name() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .ok()
        .filter(|name| valid_node_name(name))
        .unwrap_or_else(|| "Slop Conductor node".to_owned())
}

fn valid_node_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_NODE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

fn random_node_id() -> StoreResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| StoreError::Unavailable)?;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").map_err(|_| StoreError::Unavailable)?;
    }
    Ok(output)
}

fn query_node(connection: &Connection) -> StoreResult<NodeResponse> {
    connection
        .query_row(
            "SELECT node_id, name, os FROM node_identity WHERE singleton = 1",
            [],
            |row| {
                Ok(NodeResponse {
                    node_id: row.get(0)?,
                    name: row.get(1)?,
                    os: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::Database)
        .and_then(|identity| {
            validate_identity(&identity)?;
            Ok(identity)
        })
}

fn valid_command_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_COMMAND_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn page_limit(limit: usize) -> StoreResult<usize> {
    let limit = if limit == 0 { DEFAULT_PAGE_SIZE } else { limit };
    if limit > MAX_PAGE_SIZE {
        Err(StoreError::Invalid)
    } else {
        Ok(limit)
    }
}

fn opaque_id() -> StoreResult<String> {
    random_node_id()
}

fn canonical<T: serde::Serialize>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(|_| StoreError::Invalid)
}

fn prior_receipt(
    connection: &Connection,
    command_id: &str,
    scope: &str,
    payload: &str,
) -> StoreResult<Option<CommandReceipt>> {
    let saved = connection
        .query_row(
            "SELECT scope,payload,receipt FROM commands WHERE command_id=?1",
            [command_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    match saved {
        None => Ok(None),
        Some((saved_scope, saved_payload, receipt))
            if saved_scope == scope && saved_payload == payload =>
        {
            serde_json::from_str(&receipt)
                .map(Some)
                .map_err(|_| StoreError::Database)
        }
        Some(_) => Err(StoreError::Conflict),
    }
}

fn commit_command(
    tx: &Transaction<'_>,
    command_id: &str,
    scope: &str,
    payload: &str,
    receipt: &CommandReceipt,
) -> StoreResult<()> {
    let receipt = canonical(receipt)?;
    tx.execute(
        "INSERT INTO commands(command_id,scope,payload,receipt) VALUES(?1,?2,?3,?4)",
        rusqlite::params![command_id, scope, payload, receipt],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

fn append_event(
    tx: &Transaction<'_>,
    session_id: &str,
    kind: &str,
    turn_id: Option<&str>,
    message_id: Option<&str>,
) -> StoreResult<(u64, u64)> {
    tx.execute("UPDATE sessions SET revision=revision+1,last_event_sequence=last_event_sequence+1 WHERE id=?1",[session_id])
        .map_err(|_| StoreError::Database)?;
    if tx.changes() != 1 {
        return Err(StoreError::NotFound);
    }
    let (revision, sequence): (i64, i64) = tx
        .query_row(
            "SELECT revision,last_event_sequence FROM sessions WHERE id=?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| StoreError::Database)?;
    tx.execute("INSERT INTO session_events(session_id,sequence,kind,turn_id,message_id,revision) VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![session_id,sequence,kind,turn_id,message_id,revision]).map_err(|_|StoreError::Database)?;
    orchestration::session_event(tx, session_id, kind, turn_id, sequence as u64)?;
    Ok((revision as u64, sequence as u64))
}

fn make_receipt(
    command_id: &str,
    session_id: &str,
    turn_id: Option<String>,
    message_id: Option<String>,
    revision: u64,
    event_sequence: u64,
) -> CommandReceipt {
    CommandReceipt {
        command_id: command_id.to_owned(),
        session_id: session_id.to_owned(),
        turn_id,
        message_id,
        revision,
        event_sequence,
    }
}

fn create_session(
    connection: &Connection,
    request: CreateSessionRequest,
    default_max_tokens: Option<u32>,
    resolved: Option<projects::ResolvedWorkspace>,
) -> StoreResult<CommandReceipt> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let receipt = create_session_tx(&tx, request, default_max_tokens, resolved)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

fn create_session_tx(
    tx: &Transaction<'_>,
    request: CreateSessionRequest,
    default_max_tokens: Option<u32>,
    resolved: Option<projects::ResolvedWorkspace>,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id)
        || request.provider.trim().is_empty()
        || request.provider.len() > 128
        || request.model.trim().is_empty()
        || request.model.len() > 256
        || request
            .title
            .as_ref()
            .is_some_and(|title| title.len() > 256 || title.chars().any(char::is_control))
        || request
            .max_tokens
            .is_some_and(|tokens| tokens == 0 || tokens > 65_536)
        || default_max_tokens.is_some_and(|tokens| tokens == 0 || tokens > 65_536)
    {
        return Err(StoreError::Invalid);
    }
    let scope = "create_session";
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(tx, &request.command_id, scope, &payload)? {
        return Ok(receipt);
    }
    let provider_id = match request.provider.parse::<slop_core::provider::ProviderId>() {
        Ok(provider) => provider,
        Err(_) => return Err(StoreError::Invalid),
    };
    let Some(provider) = slop_runtime::providers::provider(provider_id) else {
        return Err(StoreError::Invalid);
    };
    if provider.wire_protocol(&request.model).is_err() {
        return Err(StoreError::Invalid);
    }
    let node = query_node(tx)?;
    let session_id = opaque_id()?;
    let settings = execution::session_settings(&request, default_max_tokens)?;
    let effective_max_tokens = settings
        .max_output_tokens
        .or(default_max_tokens)
        .unwrap_or(4096);
    if request.project.is_some() && request.execution.is_some() {
        return Err(StoreError::Invalid);
    }
    let managed = request
        .project
        .as_ref()
        .map(|project| {
            projects::reserve(
                tx,
                &session_id,
                project,
                resolved.as_ref().ok_or(StoreError::Invalid)?,
            )
        })
        .transpose()?;
    let policy = if let Some((_, policy)) = &managed {
        Some(policy.clone())
    } else {
        execution::workspace(&request)?
    };
    let order: i64 = tx
        .query_row(
            "SELECT next_order FROM session_order WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    tx.execute(
        "UPDATE session_order SET next_order=next_order+1 WHERE singleton=1",
        [],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute("INSERT INTO sessions(id,owner_node_id,title,provider,model,max_tokens,created_order) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        rusqlite::params![session_id,node.node_id,request.title,request.provider,request.model,effective_max_tokens,order]).map_err(|_|StoreError::Database)?;
    tx.execute(
        "UPDATE sessions SET settings=?1,execution=?2,workspace_root=?3 WHERE id=?4",
        rusqlite::params![
            canonical(&settings)?,
            policy.as_ref().map(canonical).transpose()?,
            policy.as_ref().map(|p| p.root.as_str()),
            session_id
        ],
    )
    .map_err(|_| StoreError::Database)?;
    let (mut revision, mut sequence) =
        append_event(tx, &session_id, "session_created", None, None)?;
    if let Some((workspace, _)) = managed {
        (revision, sequence) = projects::insert_workspace(tx, &workspace)?;
    }
    let receipt = make_receipt(
        &request.command_id,
        &session_id,
        None,
        None,
        revision,
        sequence,
    );
    commit_command(tx, &request.command_id, scope, &payload, &receipt)?;
    Ok(receipt)
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionResponse> {
    let settings: slop_protocol::execution::GenerationSettings = execution::json_column(row, 8)?;
    Ok(SessionResponse {
        id: row.get(0)?,
        owner_node_id: row.get(1)?,
        title: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        max_tokens: settings.max_output_tokens,
        revision: row.get::<_, i64>(6)? as u64,
        last_event_sequence: row.get::<_, i64>(7)? as u64,
        settings,
        execution: execution::optional_json_column(row, 9)?,
        managed_workspace_id: row.get(10)?,
    })
}

fn get_session(connection: &Connection, id: &str) -> StoreResult<SessionResponse> {
    connection.query_row("SELECT id,owner_node_id,title,provider,model,max_tokens,revision,last_event_sequence,settings,execution,managed_workspace_id FROM sessions WHERE id=?1",[id],session_from_row)
        .optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)
}

fn list_sessions(
    connection: &Connection,
    after: Option<u64>,
    limit: usize,
) -> StoreResult<Page<SessionResponse>> {
    let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT id,owner_node_id,title,provider,model,max_tokens,revision,last_event_sequence,settings,execution,managed_workspace_id,created_order FROM sessions WHERE created_order>?1 ORDER BY created_order LIMIT ?2")
        .map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            break;
        }
        let item = session_from_row(row).map_err(|_| StoreError::Database)?;
        next_after = Some(row.get::<_, i64>(11).map_err(|_| StoreError::Database)? as u64);
        items.push(item);
    }
    let has_more = items.len() == limit
        && connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE created_order>?1)",
                [next_after.unwrap_or(after as u64) as i64],
                |r| r.get::<_, bool>(0),
            )
            .map_err(|_| StoreError::Database)?;
    if !has_more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}

fn send_message(
    connection: &Connection,
    session_id: &str,
    request: SendMessageRequest,
    effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    idle_failure: Option<(&'static str, &'static str)>,
) -> StoreResult<CommandReceipt> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let receipt = send_message_tx(&tx, session_id, request, effective_settings, idle_failure)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

fn send_message_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    request: SendMessageRequest,
    effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    idle_failure: Option<(&'static str, &'static str)>,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id)
        || request.text.trim().is_empty()
        || request.text.len() > MAX_MESSAGE_BYTES
        || request.text.chars().any(|c| c == '\0')
    {
        return Err(StoreError::Invalid);
    }
    let scope = format!("session:{session_id}:message");
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(tx, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let session = get_session(tx, session_id)?;
    projects::require_available(tx, &session)?;
    if request
        .expected_revision
        .is_some_and(|revision| revision != session.revision)
    {
        return Err(StoreError::Conflict);
    }
    let delivery = request.delivery.unwrap_or_default();
    if delivery != DeliveryMode::AfterTurn {
        let active_turn: Option<String> = tx
            .query_row(
                "SELECT id FROM turns t WHERE session_id=?1 AND (status='running' OR status='paused' OR (status='queued' AND EXISTS(SELECT 1 FROM model_requests r WHERE r.turn_id=t.id))) ORDER BY ordinal LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StoreError::Database)?;
        if let Some(active_turn) = active_turn {
            if request.model.is_some() || request.settings.is_some() {
                return Err(StoreError::ActiveTurnOverride);
            }
            let pending: i64 = tx
                .query_row(
                    "SELECT count(*) FROM steering_instructions WHERE turn_id=?1 AND status='accepted'",
                    [&active_turn],
                    |row| row.get(0),
                )
                .map_err(|_| StoreError::Database)?;
            if pending >= MAX_PENDING_STEERING_PER_TURN {
                return Err(StoreError::Limit);
            }
            let instruction_id = opaque_id()?;
            tx.execute(
                "INSERT INTO steering_instructions(id,command_id,session_id,turn_id,text,delivery,status,created_order) VALUES(?1,?2,?3,?4,?5,?6,'accepted',(SELECT COALESCE(MAX(created_order),0)+1 FROM steering_instructions))",
                rusqlite::params![
                    instruction_id,
                    request.command_id,
                    session_id,
                    active_turn,
                    request.text,
                    match delivery {
                        DeliveryMode::NextBoundary => "next-boundary",
                        DeliveryMode::Immediate => "immediate",
                        DeliveryMode::AfterTurn => unreachable!(),
                    }
                ],
            )
            .map_err(|_| StoreError::Database)?;
            if delivery == DeliveryMode::Immediate {
                tx.execute(
                    "UPDATE turns SET immediate_requested=1 WHERE id=?1 AND status='running'",
                    [&active_turn],
                )
                .map_err(|_| StoreError::Database)?;
            }
            let (revision, event_sequence) = append_event(
                tx,
                session_id,
                "steering_instruction_accepted",
                Some(&active_turn),
                Some(&instruction_id),
            )?;
            let receipt = make_receipt(
                &request.command_id,
                session_id,
                Some(active_turn),
                Some(instruction_id),
                revision,
                event_sequence,
            );
            commit_command(tx, &request.command_id, &scope, &payload, &receipt)?;
            return Ok(receipt);
        }
    }
    if tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id=?1)",
            [session_id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|_| StoreError::Database)?
    {
        return Err(StoreError::Conflict);
    }
    if let Some((code, message)) = idle_failure {
        return Err(StoreError::Preflight(code, message));
    }
    let queued_session: i64 = tx
        .query_row(
            "SELECT count(*) FROM turns WHERE session_id=?1 AND status='queued'",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let queued_global: i64 = tx
        .query_row(
            "SELECT count(*) FROM turns WHERE status='queued'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if queued_session >= MAX_QUEUE_PER_SESSION || queued_global >= MAX_QUEUE_GLOBAL {
        return Err(StoreError::Limit);
    }
    let turn_id = opaque_id()?;
    let message_id = opaque_id()?;
    let ordinal: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(ordinal),0)+1 FROM turns WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let user_order = ordinal.checked_mul(1024).ok_or(StoreError::Limit)?;
    let requested_model = request
        .model
        .clone()
        .unwrap_or_else(|| format!("{}/{}", session.provider, session.model));
    let settings = match effective_settings {
        Some(settings) => {
            execution::validate_settings(&requested_model, &settings)?;
            settings
        }
        None => execution::turn_settings(&session, &request, &requested_model)?,
    };
    tx.execute("INSERT INTO turns(id,session_id,ordinal,user_message_id,status,requested_model) VALUES(?1,?2,?3,?4,'queued',?5)",rusqlite::params![turn_id,session_id,ordinal,message_id,requested_model]).map_err(|_|StoreError::Database)?;
    tx.execute(
        "UPDATE turns SET settings=?1,requested_settings=?2 WHERE id=?3",
        rusqlite::params![
            canonical(&settings)?,
            canonical(&request.settings.clone().unwrap_or_default())?,
            turn_id
        ],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'user',?5,'completed')",rusqlite::params![message_id,session_id,turn_id,user_order,request.text]).map_err(|_|StoreError::Database)?;
    let (revision, event_sequence) = append_event(
        tx,
        session_id,
        "user_message_accepted",
        Some(&turn_id),
        Some(&message_id),
    )?;
    let receipt = make_receipt(
        &request.command_id,
        session_id,
        Some(turn_id),
        Some(message_id),
        revision,
        event_sequence,
    );
    commit_command(tx, &request.command_id, &scope, &payload, &receipt)?;
    Ok(receipt)
}

fn list_messages(
    connection: &Connection,
    session_id: &str,
    after: Option<u64>,
    limit: usize,
) -> StoreResult<Page<MessageResponse>> {
    let _ = get_session(connection, session_id)?;
    let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT id,session_id,turn_id,role,text,status,blocks,request_id,ordinal FROM messages WHERE session_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3").map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![session_id, after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    let mut encoded_bytes = 128usize;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            has_more = true;
            break;
        }
        let item = MessageResponse {
            id: row.get(0).map_err(|_| StoreError::Database)?,
            session_id: row.get(1).map_err(|_| StoreError::Database)?,
            turn_id: row.get(2).map_err(|_| StoreError::Database)?,
            role: row.get(3).map_err(|_| StoreError::Database)?,
            text: row.get(4).map_err(|_| StoreError::Database)?,
            status: row.get(5).map_err(|_| StoreError::Database)?,
            blocks: execution::public_blocks_from_row(row, 6, 0, 4)?,
            request_id: row.get(7).map_err(|_| StoreError::Database)?,
        };
        let item_bytes = serde_json::to_vec(&item)
            .map_err(|_| StoreError::Database)?
            .len();
        if encoded_bytes.saturating_add(item_bytes).saturating_add(1) > MAX_MESSAGE_PAGE_BYTES {
            if items.is_empty() {
                return Err(StoreError::Limit);
            }
            has_more = true;
            break;
        }
        encoded_bytes += item_bytes + usize::from(!items.is_empty());
        next_after = Some(row.get::<_, i64>(8).map_err(|_| StoreError::Database)? as u64);
        items.push(item);
    }
    if has_more {
        // Keep the cursor after the last included row. The next page may fit a
        // large message that was deferred solely by the encoded-size bound.
    } else if next_after.is_some_and(|cursor| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND ordinal>?2)",
                rusqlite::params![session_id, cursor as i64],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }) {
        has_more = true;
    }
    if !has_more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}

fn list_events(
    connection: &Connection,
    session_id: &str,
    after: u64,
    limit: usize,
) -> StoreResult<Page<EventResponse>> {
    let _ = get_session(connection, session_id)?;
    let after = after.min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT session_id,sequence,kind,turn_id,message_id,revision,request_id,invocation_id FROM session_events WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![session_id, after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            break;
        }
        let sequence: i64 = row.get(1).map_err(|_| StoreError::Database)?;
        items.push(EventResponse {
            session_id: row.get(0).map_err(|_| StoreError::Database)?,
            sequence: sequence as u64,
            kind: row.get(2).map_err(|_| StoreError::Database)?,
            turn_id: row.get(3).map_err(|_| StoreError::Database)?,
            message_id: row.get(4).map_err(|_| StoreError::Database)?,
            revision: row.get::<_, i64>(5).map_err(|_| StoreError::Database)? as u64,
            request_id: row.get(6).map_err(|_| StoreError::Database)?,
            invocation_id: row.get(7).map_err(|_| StoreError::Database)?,
        });
        next_after = Some(sequence as u64);
    }
    let more = next_after.is_some_and(|cursor| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_events WHERE session_id=?1 AND sequence>?2)",
                rusqlite::params![session_id, cursor as i64],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    });
    if !more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}

fn get_base_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    connection.query_row("SELECT id,session_id,user_message_id,assistant_message_id,status,requested_model,resolved_model,input_tokens,output_tokens,total_tokens,total_source,error_code,error_message FROM turns WHERE id=?1",[id],|r|{let input:Option<String>=r.get(7)?;let output:Option<String>=r.get(8)?;let total:Option<String>=r.get(9)?;let total_source:Option<String>=r.get(10)?;Ok(TurnResponse{id:r.get(0)?,session_id:r.get(1)?,user_message_id:r.get(2)?,assistant_message_id:r.get(3)?,status:r.get(4)?,requested_model:r.get(5)?,resolved_model:r.get(6)?,usage:if input.is_some()||output.is_some()||total.is_some()||total_source.is_some(){Some(UsageResponse{input_tokens:input.and_then(|v|v.parse().ok()),output_tokens:output.and_then(|v|v.parse().ok()),total_tokens:total.and_then(|v|v.parse().ok()),total_source})}else{None},error_code:r.get(11)?,error_message:r.get(12)?,settings:Default::default(),model_request_ids:Vec::new(),tool_invocation_ids:Vec::new()})}).optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)
}

fn cancel_turn(
    connection: &Connection,
    turn_id: &str,
    request: CancelTurnRequest,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id) {
        return Err(StoreError::Invalid);
    }
    let scope = format!("turn:{turn_id}:cancel");
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(connection, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let (session_id, status): (String, String) = tx
        .query_row(
            "SELECT session_id,status FROM turns WHERE id=?1",
            [turn_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    let kind = match status.as_str() {
        "queued" | "paused" => {
            tx.execute("UPDATE turns SET status='cancelled' WHERE id=?1", [turn_id])
                .map_err(|_| StoreError::Database)?;
            reject_steering_tx(&tx, turn_id, "turn_cancelled")?;
            "turn_cancelled"
        }
        "running" => {
            tx.execute(
                "UPDATE turns SET cancellation_requested=1 WHERE id=?1",
                [turn_id],
            )
            .map_err(|_| StoreError::Database)?;
            "turn_cancel_requested"
        }
        "completed" | "failed" | "cancelled" | "interrupted" | "incomplete" => {
            return Err(StoreError::Conflict);
        }
        _ => return Err(StoreError::Database),
    };
    let (revision, event_sequence) = append_event(&tx, &session_id, kind, Some(turn_id), None)?;
    let receipt = make_receipt(
        &request.command_id,
        &session_id,
        Some(turn_id.to_owned()),
        None,
        revision,
        event_sequence,
    );
    commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

fn recover_interrupted(connection: &Connection) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let mut stmt = tx
        .prepare("SELECT id,session_id FROM turns WHERE status='running'")
        .map_err(|_| StoreError::Database)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|_| StoreError::Database)?;
    let active: Vec<(String, String)> = rows
        .collect::<Result<_, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for (turn_id, session_id) in active {
        execution::reconcile_tools(&tx, &turn_id, "daemon_restarted")?;
        execution::reconcile_requests(&tx, &turn_id, "daemon_restarted")?;
        // Incomplete thinking is discarded; completed tool steps remain facts.
        tx.execute(
            "UPDATE messages SET status='interrupted',continuation=NULL WHERE turn_id=?1 AND role='assistant' AND status!='completed'",
            [&turn_id],
        )
        .map_err(|_| StoreError::Database)?;
        tx.execute("UPDATE turns SET status='interrupted',error_code='daemon_restarted',error_message='The daemon restarted during this turn.' WHERE id=?1",[&turn_id]).map_err(|_|StoreError::Database)?;
        reject_steering_tx(&tx, &turn_id, "daemon_restarted")?;
        append_event(&tx, &session_id, "turn_interrupted", Some(&turn_id), None)?;
    }
    projects::recover(&tx)?;
    tx.commit().map_err(|_| StoreError::Database)
}

fn turn_control(
    connection: &Connection,
    turn_id: &str,
    request: TurnControlRequest,
    action: &'static str,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id) {
        return Err(StoreError::Invalid);
    }
    let scope = format!("turn:{turn_id}:{action}");
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(connection, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let (session_id, status, resume_count): (String, String, i64) = tx
        .query_row(
            "SELECT session_id,status,resume_count FROM turns WHERE id=?1",
            [turn_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    let kind = match (action, status.as_str()) {
        ("pause", "running") => {
            tx.execute("UPDATE turns SET pause_requested=1 WHERE id=?1", [turn_id])
                .map_err(|_| StoreError::Database)?;
            "turn_pause_requested"
        }
        ("pause", "queued") => {
            tx.execute("UPDATE turns SET status='paused' WHERE id=?1", [turn_id])
                .map_err(|_| StoreError::Database)?;
            "turn_paused"
        }
        ("resume", "paused") if resume_count < 64 => {
            // A paused turn is already at a durable safe boundary. Apply
            // accepted inbox entries before explicit readmission.
            apply_steering_tx(&tx, turn_id)?;
            tx.execute(
                "UPDATE turns SET status='queued',pause_requested=0,resume_count=resume_count+1 WHERE id=?1",
                [turn_id],
            )
            .map_err(|_| StoreError::Database)?;
            "turn_resumed"
        }
        _ => return Err(StoreError::Conflict),
    };
    let (revision, event_sequence) = append_event(&tx, &session_id, kind, Some(turn_id), None)?;
    let receipt = make_receipt(
        &request.command_id,
        &session_id,
        Some(turn_id.to_owned()),
        None,
        revision,
        event_sequence,
    );
    commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

fn list_steering(
    connection: &Connection,
    session_id: &str,
    after: i64,
    limit: usize,
) -> StoreResult<Page<SteeringInstructionResponse>> {
    let _ = get_session(connection, session_id)?;
    let mut statement = connection
        .prepare("SELECT id,turn_id,text,delivery,status,error_code,created_order FROM steering_instructions WHERE session_id=?1 AND created_order>?2 ORDER BY created_order LIMIT ?3")
        .map_err(|_| StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![session_id, after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            break;
        }
        let delivery: String = row.get(3).map_err(|_| StoreError::Database)?;
        items.push(SteeringInstructionResponse {
            id: row.get(0).map_err(|_| StoreError::Database)?,
            turn_id: row.get(1).map_err(|_| StoreError::Database)?,
            text: row.get(2).map_err(|_| StoreError::Database)?,
            delivery: match delivery.as_str() {
                "next-boundary" => DeliveryMode::NextBoundary,
                "immediate" => DeliveryMode::Immediate,
                _ => return Err(StoreError::Database),
            },
            status: row.get(4).map_err(|_| StoreError::Database)?,
            error_code: row.get(5).map_err(|_| StoreError::Database)?,
        });
        next_after = Some(row.get::<_, i64>(6).map_err(|_| StoreError::Database)? as u64);
    }
    let more = next_after.is_some_and(|cursor| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM steering_instructions WHERE session_id=?1 AND created_order>?2)",
                rusqlite::params![session_id, cursor as i64],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    });
    if !more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}

fn turn_control_state(
    connection: &Connection,
    turn_id: &str,
) -> StoreResult<slop_runtime::chat::TurnControl> {
    connection
        .query_row(
            "SELECT cancellation_requested,immediate_requested,pause_requested,EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='accepted') FROM turns WHERE id=?1",
            [turn_id],
            |row| {
                Ok(slop_runtime::chat::TurnControl {
                    cancellation_requested: row.get::<_, i64>(0)? != 0,
                    immediate_requested: row.get::<_, i64>(1)? != 0,
                    pause_requested: row.get::<_, i64>(2)? != 0,
                    steering_pending: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)
}

fn apply_steering_tx(tx: &Transaction<'_>, turn_id: &str) -> StoreResult<usize> {
    let session_id: String = tx
        .query_row(
            "SELECT session_id FROM turns WHERE id=?1",
            [turn_id],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let mut statement = tx
        .prepare("SELECT id,text FROM steering_instructions WHERE turn_id=?1 AND status='accepted' ORDER BY created_order LIMIT ?2")
        .map_err(|_| StoreError::Database)?;
    let rows = statement
        .query_map(
            rusqlite::params![turn_id, MAX_PENDING_STEERING_PER_TURN],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(|_| StoreError::Database)?;
    let instructions: Vec<_> = rows
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| StoreError::Database)?;
    drop(statement);
    for (id, text) in &instructions {
        let ordinal = execution::next_message_ordinal(tx, turn_id)?;
        tx.execute(
            "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'user',?5,'completed')",
            rusqlite::params![id, session_id, turn_id, ordinal, text],
        )
        .map_err(|_| StoreError::Database)?;
        tx.execute(
            "UPDATE steering_instructions SET status='applied' WHERE id=?1 AND status='accepted'",
            [id],
        )
        .map_err(|_| StoreError::Database)?;
        append_event(
            tx,
            &session_id,
            "steering_instruction_applied",
            Some(turn_id),
            Some(id),
        )?;
    }
    tx.execute(
        "UPDATE turns SET immediate_requested=0 WHERE id=?1",
        [turn_id],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(instructions.len())
}

fn reject_steering_tx(tx: &Transaction<'_>, turn_id: &str, cause: &str) -> StoreResult<()> {
    let session_id: String = tx
        .query_row(
            "SELECT session_id FROM turns WHERE id=?1",
            [turn_id],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let mut statement = tx
        .prepare("SELECT id FROM steering_instructions WHERE turn_id=?1 AND status='accepted' ORDER BY created_order")
        .map_err(|_| StoreError::Database)?;
    let rows = statement
        .query_map([turn_id], |row| row.get::<_, String>(0))
        .map_err(|_| StoreError::Database)?;
    let ids: Vec<String> = rows
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| StoreError::Database)?;
    drop(statement);
    for id in ids {
        tx.execute(
            "UPDATE steering_instructions SET status='rejected',error_code=?1 WHERE id=?2",
            rusqlite::params![cause, id],
        )
        .map_err(|_| StoreError::Database)?;
        append_event(
            tx,
            &session_id,
            "steering_instruction_rejected",
            Some(turn_id),
            Some(&id),
        )?;
    }
    tx.execute(
        "UPDATE turns SET immediate_requested=0 WHERE id=?1",
        [turn_id],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

fn claim_next_turn(connection: &Connection) -> StoreResult<Option<slop_runtime::chat::TurnWork>> {
    execution::claim_next_turn(connection)
}

fn get_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    execution::get_turn(connection, id)
}

fn checkpoint_turn(connection: &Connection, turn_id: &str, text: &str) -> StoreResult<()> {
    if text.len() > MAX_CONTEXT_BYTES {
        return Err(StoreError::Limit);
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let (session_id, status, cancellation_requested, existing): (String, String, bool, Option<String>) = tx
        .query_row(
            "SELECT session_id,status,cancellation_requested,assistant_message_id FROM turns WHERE id=?1",
            [turn_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_,i64>(2)?!=0, r.get(3)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    if status != "running" || cancellation_requested {
        return Err(StoreError::Conflict);
    }
    let was_existing = existing.is_some();
    let message_id = if let Some(id) = existing {
        id
    } else {
        let id = opaque_id()?;
        let ordinal = execution::next_message_ordinal(&tx, turn_id)?;
        tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'assistant',?5,'checkpoint')",rusqlite::params![id,session_id,turn_id,ordinal,text]).map_err(|_|StoreError::Database)?;
        tx.execute(
            "UPDATE turns SET assistant_message_id=?1 WHERE id=?2",
            rusqlite::params![id, turn_id],
        )
        .map_err(|_| StoreError::Database)?;
        id
    };
    if was_existing {
        tx.execute(
            "UPDATE messages SET text=?1,status='checkpoint' WHERE id=?2",
            rusqlite::params![text, message_id],
        )
        .map_err(|_| StoreError::Database)?;
    }
    append_event(
        &tx,
        &session_id,
        "assistant_message_checkpointed",
        Some(turn_id),
        Some(&message_id),
    )?;
    tx.commit().map_err(|_| StoreError::Database)
}

fn finish_turn(
    connection: &Connection,
    turn_id: &str,
    mut outcome: slop_runtime::chat::ChatOutcome,
) -> StoreResult<()> {
    use slop_runtime::chat::ChatStatus;
    if outcome.text.len() > MAX_CONTEXT_BYTES {
        outcome.text.clear();
        outcome.status = ChatStatus::Failed;
        outcome.error_code = Some("output_limit".to_owned());
        outcome.error_message = Some("Assistant output exceeds the supported bound.".to_owned());
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let (session_id,status,cancel_requested,pause_requested,already_unknown,existing):(String,String,bool,bool,bool,Option<String>)=tx.query_row("SELECT session_id,status,cancellation_requested,pause_requested,usage_unknown,assistant_message_id FROM turns WHERE id=?1",[turn_id],|r|Ok((r.get(0)?,r.get(1)?,r.get::<_,i64>(2)?!=0,r.get::<_,i64>(3)?!=0,r.get::<_,i64>(4)?!=0,r.get(5)?))).optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)?;
    if status != "running" {
        return Err(StoreError::Conflict);
    }
    let pending_steering: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='accepted')",
            [turn_id],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let steering_yield = matches!(
        outcome.error_code.as_deref(),
        Some("steering_resume" | "steering_resume_unknown")
    ) || pending_steering && outcome.status == ChatStatus::Completed;
    let pause_yield = outcome.error_code.as_deref() == Some("pause_turn")
        || pause_requested && outcome.status == ChatStatus::Completed;
    if !cancel_requested && (steering_yield || pause_yield) {
        execution::reconcile_requests(&tx, turn_id, "steering_interrupted")?;
        execution::reconcile_tools(&tx, turn_id, "steering_applied_before_dispatch")?;
        apply_steering_tx(&tx, turn_id)?;
        let paused = pause_yield || pause_requested;
        if outcome.status == ChatStatus::Completed
            && let Some(message_id) = &existing
        {
            tx.execute(
                "UPDATE messages SET text=?1,status='completed' WHERE id=?2",
                rusqlite::params![outcome.text, message_id],
            )
            .map_err(|_| StoreError::Database)?;
        }
        let unknown_now = outcome.error_code.as_deref() == Some("steering_resume_unknown");
        let usage_unknown = already_unknown || unknown_now;
        let usage = if usage_unknown {
            slop_runtime::providers::Usage::default()
        } else {
            outcome.usage
        };
        use slop_runtime::providers::UsageSource;
        let input = usage.input_tokens.map(|value| value.to_string());
        let output = usage.output_tokens.map(|value| value.to_string());
        let total = usage.total_tokens.map(|value| value.to_string());
        let source = usage.total_source.map(|source| match source {
            UsageSource::Reported => "reported",
            UsageSource::Derived => "derived",
        });
        tx.execute(
            "UPDATE turns SET status=?1,input_tokens=?2,output_tokens=?3,total_tokens=?4,total_source=?5,usage_unknown=?6,pause_requested=0,error_code=NULL,error_message=NULL WHERE id=?7",
            rusqlite::params![
                if paused { "paused" } else { "queued" },
                input,
                output,
                total,
                source,
                usage_unknown,
                turn_id
            ],
        )
        .map_err(|_| StoreError::Database)?;
        append_event(
            &tx,
            &session_id,
            if paused {
                "turn_paused"
            } else {
                "turn_steering_yielded"
            },
            Some(turn_id),
            existing.as_deref(),
        )?;
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(());
    }
    if cancel_requested {
        outcome.status = ChatStatus::Canceled;
        outcome.text.clear();
        outcome.resolved_model = None;
        reject_steering_tx(&tx, turn_id, "turn_cancelled")?;
    } else if outcome.status != ChatStatus::Completed {
        reject_steering_tx(
            &tx,
            turn_id,
            outcome.error_code.as_deref().unwrap_or("turn_failed"),
        )?;
    }
    if outcome.status != ChatStatus::Completed {
        execution::reconcile_requests(
            &tx,
            turn_id,
            outcome.error_code.as_deref().unwrap_or("turn_cancelled"),
        )?;
        execution::reconcile_tools(
            &tx,
            turn_id,
            outcome.error_code.as_deref().unwrap_or("turn_cancelled"),
        )?;
    }
    let event_kind = match outcome.status {
        ChatStatus::Completed => "turn_completed",
        ChatStatus::Canceled => "turn_cancelled",
        ChatStatus::Incomplete => "turn_incomplete",
        ChatStatus::Interrupted => "turn_interrupted",
        ChatStatus::Failed => "turn_failed",
    };
    let final_status = match outcome.status {
        ChatStatus::Completed => "completed",
        ChatStatus::Canceled => "cancelled",
        ChatStatus::Incomplete => "incomplete",
        ChatStatus::Interrupted => "interrupted",
        ChatStatus::Failed => "failed",
    };
    let message_status = if outcome.status == ChatStatus::Completed {
        "completed"
    } else if outcome.status == ChatStatus::Failed {
        "failed"
    } else if outcome.status == ChatStatus::Incomplete {
        "incomplete"
    } else {
        "interrupted"
    };
    let preserve_completed = if let Some(id) = &existing {
        tx.query_row(
            "SELECT status='completed' AND request_id IS NOT NULL FROM messages WHERE id=?1",
            [id],
            |r| r.get::<_, bool>(0),
        )
        .map_err(|_| StoreError::Database)?
    } else {
        false
    };
    let assistant_id = if preserve_completed && outcome.status != ChatStatus::Completed {
        existing
    } else if outcome.status == ChatStatus::Completed
        || existing.is_some()
        || !outcome.text.is_empty()
    {
        let id = if let Some(id) = existing {
            id
        } else {
            let id = opaque_id()?;
            let ordinal = execution::next_message_ordinal(&tx, turn_id)?;
            tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'assistant',?5,?6)",rusqlite::params![id,session_id,turn_id,ordinal,outcome.text,message_status]).map_err(|_|StoreError::Database)?;
            id
        };
        if outcome.text.is_empty() && outcome.status != ChatStatus::Completed {
            tx.execute(
                "UPDATE messages SET status=?1 WHERE id=?2",
                rusqlite::params![message_status, id],
            )
            .map_err(|_| StoreError::Database)?;
        } else {
            tx.execute(
                "UPDATE messages SET text=?1,status=?2 WHERE id=?3",
                rusqlite::params![outcome.text, message_status, id],
            )
            .map_err(|_| StoreError::Database)?;
        }
        Some(id)
    } else {
        None
    };
    use slop_runtime::providers::UsageSource;
    let input = outcome.usage.input_tokens.map(|value| value.to_string());
    let output = outcome.usage.output_tokens.map(|value| value.to_string());
    let total = outcome.usage.total_tokens.map(|value| value.to_string());
    let total_source = outcome.usage.total_source.map(|source| match source {
        UsageSource::Reported => "reported",
        UsageSource::Derived => "derived",
    });
    tx.execute("UPDATE turns SET status=?1,assistant_message_id=?2,resolved_model=?3,input_tokens=?4,output_tokens=?5,total_tokens=?6,total_source=?7,error_code=?8,error_message=?9 WHERE id=?10",
        rusqlite::params![final_status,assistant_id,outcome.resolved_model,input,output,total,total_source,outcome.error_code,outcome.error_message,turn_id]).map_err(|_|StoreError::Database)?;
    append_event(
        &tx,
        &session_id,
        event_kind,
        Some(turn_id),
        assistant_id.as_deref(),
    )?;
    tx.commit().map_err(|_| StoreError::Database)
}

fn cancellation_requested(connection: &Connection, turn_id: &str) -> StoreResult<bool> {
    connection
        .query_row(
            "SELECT cancellation_requested FROM turns WHERE id=?1",
            [turn_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .map(|value| value != 0)
        .ok_or(StoreError::NotFound)
}

impl slop_runtime::chat::ChatRepository for StoreClient {
    fn prepare_workspace<'a>(
        &'a self,
        turn_id: &'a str,
        shutdown: &'a mut tokio::sync::watch::Receiver<bool>,
    ) -> slop_runtime::chat::RepoFuture<'a, Option<(&'static str, &'static str)>> {
        Box::pin(async move {
            self.prepare_workspace(turn_id, shutdown)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
    fn begin_request<'a>(
        &'a self,
        turn_id: &'a str,
        intent: slop_runtime::agent::RequestIntent,
    ) -> slop_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            self.submit(move |c| execution::begin_request(c, &turn, intent))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn checkpoint_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        message: slop_runtime::providers::inference::InferenceMessage,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let request = request_id.to_owned();
            self.submit(move |c| execution::checkpoint_request(c, &turn, &request, message))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn complete_request<'a>(
        &'a self,
        turn_id: &'a str,
        completion: slop_runtime::agent::RequestCompletion,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            self.submit(move |c| execution::complete_request(c, &turn, completion))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn fail_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        code: &'a str,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let request = request_id.to_owned();
            let code = code.to_owned();
            self.submit(move |c| execution::fail_request(c, &turn, &request, &code))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn start_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a slop_runtime::agent::ToolIntent,
    ) -> slop_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let intent = intent.clone();
            self.submit(move |c| execution::start_tool(c, &turn, &intent))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn finish_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a slop_runtime::agent::ToolIntent,
        outcome: slop_runtime::tools::ToolOutcome,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let intent = intent.clone();
            self.submit(move |c| execution::finish_tool(c, &turn, &intent, outcome))
                .await
                .map_err(|e| Box::new(e) as slop_runtime::chat::RepoError)
        })
    }
    fn claim_next(
        &self,
    ) -> slop_runtime::chat::RepoFuture<'_, Option<slop_runtime::chat::TurnWork>> {
        Box::pin(async move {
            if !self.shared.workspace_healthy.load(Ordering::Acquire) {
                return Err(Box::new(StoreError::Unavailable) as slop_runtime::chat::RepoError);
            }
            self.claim_next_turn()
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
    fn checkpoint_visible<'a>(
        &'a self,
        turn_id: &'a str,
        text: &'a str,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            self.checkpoint_visible(turn_id, text)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
    fn finish_turn<'a>(
        &'a self,
        turn_id: &'a str,
        outcome: slop_runtime::chat::ChatOutcome,
    ) -> slop_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            self.finish_chat_turn(turn_id, outcome)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
    fn cancellation_requested<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> slop_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            self.turn_cancellation_requested(turn_id)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
    fn turn_control<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> slop_runtime::chat::RepoFuture<'a, slop_runtime::chat::TurnControl> {
        Box::pin(async move {
            self.turn_control(turn_id)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }

    fn retain_provider_snapshot<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> slop_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            self.retain_provider_snapshot(turn_id)
                .await
                .map_err(|error| Box::new(error) as slop_runtime::chat::RepoError)
        })
    }
}

fn validate_identity(identity: &NodeResponse) -> StoreResult<()> {
    if identity.node_id.len() != 64
        || !identity
            .node_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_node_name(&identity.name)
        || identity.os.trim().is_empty()
        || identity.os.len() > MAX_NODE_NAME_BYTES
        || identity.os.chars().any(char::is_control)
    {
        return Err(StoreError::Database);
    }
    Ok(())
}

fn verify_pragmas(connection: &Connection, busy_timeout: Duration) -> StoreResult<()> {
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let busy: i64 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let expected_busy = busy_timeout.as_millis().min(i64::MAX as u128) as i64;
    if !journal.eq_ignore_ascii_case("wal")
        || synchronous != 2
        || foreign_keys != 1
        || busy != expected_busy
    {
        return Err(StoreError::Database);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn open_test(dir: &TempDir, name: Option<&str>) -> Store {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
                .expect("private temp dir");
        }
        Store::open(
            dir.path().to_path_buf(),
            name.map(str::to_owned),
            8,
            Duration::from_millis(200),
        )
        .await
        .expect("store opens")
    }

    fn create_request(command_id: &str, title: Option<&str>) -> CreateSessionRequest {
        CreateSessionRequest {
            command_id: command_id.to_owned(),
            title: title.map(str::to_owned),
            provider: "opencode-go".to_owned(),
            model: "glm-5.3-flash".to_owned(),
            max_tokens: Some(1024),

            ..Default::default()
        }
    }

    #[test]
    fn provider_defaults_preserve_settings_object_omission_and_subscription_no_cap() {
        let mut go = create_request("go-default", None);
        go.max_tokens = None;
        go.settings = Some(slop_protocol::execution::GenerationSettings::default());
        assert_eq!(
            execution::session_settings(&go, Some(4096))
                .unwrap()
                .max_output_tokens,
            Some(4096)
        );

        let mut codex = go;
        codex.provider = "codex".into();
        codex.model = "gpt-6.1-sol".into();
        assert_eq!(
            execution::session_settings(&codex, None)
                .unwrap()
                .max_output_tokens,
            None
        );
    }

    #[test]
    fn schema_two_migration_preserves_history_settings_and_command_payloads() {
        let connection = Connection::open_in_memory().unwrap();
        let tx = connection.unchecked_transaction().unwrap();
        create_chat_schema(&tx).unwrap();
        tx.execute("INSERT INTO sessions(id,owner_node_id,provider,model,max_tokens,created_order) VALUES('session','node','opencode-go','glm-5.3-flash',1024,1)",[]).unwrap();
        tx.execute(
            "INSERT INTO commands VALUES('command','scope','{\"text\":\"old payload\"}','{}')",
            [],
        )
        .unwrap();
        for i in 1..=2 {
            tx.execute("INSERT INTO turns(id,session_id,ordinal,user_message_id,assistant_message_id,status,requested_model) VALUES(?1,'session',?2,?3,?4,'completed','opencode-go/glm-5.3-flash')",rusqlite::params![format!("turn-{i}"),i,format!("user-{i}"),format!("assistant-{i}")]).unwrap();
            for (role, offset) in [("user", 0), ("assistant", 1)] {
                tx.execute(
                    "INSERT INTO messages VALUES(?1,'session',?2,?3,?4,?5,'completed')",
                    rusqlite::params![
                        format!("{role}-{i}"),
                        format!("turn-{i}"),
                        i * 2 + offset,
                        role,
                        format!("{role} {i}")
                    ],
                )
                .unwrap();
            }
        }
        tx.pragma_update(None, "user_version", 2).unwrap();
        tx.commit().unwrap();
        migrate(&connection, 2, None).unwrap();
        let history = list_messages(&connection, "session", None, 20).unwrap();
        assert_eq!(
            history
                .items
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>(),
            ["user 1", "assistant 1", "user 2", "assistant 2"]
        );
        assert!(history.items.iter().all(|m| m.blocks.len() == 1));
        assert_eq!(
            get_session(&connection, "session")
                .unwrap()
                .settings
                .max_output_tokens,
            Some(1024)
        );
        let payload: String = connection
            .query_row(
                "SELECT payload FROM commands WHERE command_id='command'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(payload, "{\"text\":\"old payload\"}");
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[tokio::test]
    async fn accepted_settings_are_frozen_and_workspace_is_leased_across_sessions() {
        use slop_protocol::execution::{GenerationSettings, WorkspacePolicy};
        let dir = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let policy = WorkspacePolicy {
            root: workspace.path().to_str().unwrap().into(),
            allowed_tools: vec!["read".into()],
            shell_timeout_ms: 1000,
            max_output_bytes: 1024,
            max_tool_calls: 4,
            max_model_requests: 4,
        };
        let mut first = create_request("lease-first", None);
        first.execution = Some(policy.clone());
        let first = client.create_session(first).await.unwrap();
        let mut second = create_request("lease-second", None);
        second.execution = Some(policy);
        let second = client.create_session(second).await.unwrap();
        let sent = client
            .send_message(
                &first.session_id,
                SendMessageRequest {
                    command_id: "selected".into(),
                    text: "hello".into(),
                    model: Some("opencode-go/glm-5.3".into()),
                    settings: Some(GenerationSettings {
                        max_output_tokens: Some(512),
                        reasoning_effort: None,
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        client
            .send_message(
                &second.session_id,
                SendMessageRequest {
                    command_id: "blocked-workspace".into(),
                    text: "wait".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, sent.turn_id.clone().unwrap());
        assert_eq!(work.requested_model, "opencode-go/glm-5.3");
        assert_eq!(work.settings.max_output_tokens, Some(512));
        assert_eq!(
            client
                .session(&first.session_id)
                .await
                .unwrap()
                .settings
                .max_output_tokens,
            Some(1024)
        );
        assert!(client.claim_next_turn().await.unwrap().is_none());
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "done".into(),
                    resolved_model: None,
                    usage: Default::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .unwrap();
        let next = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(next.session_id, second.session_id);
        assert_eq!(next.settings.max_output_tokens, Some(1024));
        let before = client.session(&first.session_id).await.unwrap().revision;
        let unsupported = client
            .send_message(
                &first.session_id,
                SendMessageRequest {
                    command_id: "effort".into(),
                    text: "reject".into(),
                    settings: Some(GenerationSettings {
                        max_output_tokens: None,
                        reasoning_effort: Some("high".into()),
                    }),
                    ..Default::default()
                },
            )
            .await;
        assert!(matches!(
            unsupported,
            Err(StoreError::Unsupported("settings.reasoning_effort"))
        ));
        assert_eq!(
            client.session(&first.session_id).await.unwrap().revision,
            before
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn boundary_steering_is_durable_idempotent_and_applied_before_readmission() {
        use slop_runtime::chat::ChatRepository;
        let dir = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("steer-create", None))
            .await
            .unwrap();
        let session = client.session(&created.session_id).await.unwrap();
        let original = SendMessageRequest {
            command_id: "steer-original".into(),
            text: "first task".into(),
            ..Default::default()
        };
        let original_receipt = client
            .send_message(&created.session_id, original.clone())
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, original_receipt.turn_id.as_deref().unwrap());
        client
            .begin_request(
                &work.turn_id,
                slop_runtime::agent::RequestIntent {
                    id: "steer-model-request".into(),
                    message_id: "steer-assistant-message".into(),
                    ordinal: 1,
                    requested_model: work.requested_model.clone(),
                    requested_settings: work.requested_settings.clone(),
                    settings: work.settings.clone(),
                },
            )
            .await
            .unwrap();

        let steering = SendMessageRequest {
            command_id: "steer-boundary".into(),
            text: "change direction".into(),
            delivery: Some(DeliveryMode::NextBoundary),
            ..Default::default()
        };
        let accepted = client
            .send_message_preflighted(
                &created.session_id,
                steering.clone(),
                session.settings.clone(),
                Some(("unsupported_capability", "idle fallback rejected")),
            )
            .await
            .unwrap();
        assert_eq!(accepted.turn_id, original_receipt.turn_id);
        let replay = client
            .send_message(&created.session_id, steering)
            .await
            .unwrap();
        assert_eq!(replay, accepted);
        assert_eq!(
            client
                .steering_instructions(&created.session_id, None, 10)
                .await
                .unwrap()
                .items[0]
                .status,
            "accepted"
        );

        client
            .finish_chat_turn(
                work.turn_id.clone().as_str(),
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: Some(work.requested_model),
                    usage: slop_runtime::providers::Usage {
                        input_tokens: Some(13),
                        output_tokens: Some(2),
                        total_tokens: Some(15),
                        total_source: Some(slop_runtime::providers::UsageSource::Reported),
                    },
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .unwrap();
        let items = client
            .steering_instructions(&created.session_id, None, 10)
            .await
            .unwrap()
            .items;
        assert_eq!(items[0].status, "applied");
        let queued_instruction = client
            .send_message_preflighted(
                &created.session_id,
                SendMessageRequest {
                    command_id: "steer-queued-second".into(),
                    text: "second direction".into(),
                    delivery: Some(DeliveryMode::Immediate),
                    ..Default::default()
                },
                session.settings.clone(),
                Some(("unsupported_capability", "idle fallback rejected")),
            )
            .await;
        assert_eq!(
            queued_instruction.unwrap().turn_id.as_deref(),
            Some(work.turn_id.as_str())
        );
        let idle_session = client
            .create_session(create_request("steer-idle-create", None))
            .await
            .unwrap();
        let idle_rejected = client
            .send_message_preflighted(
                &idle_session.session_id,
                SendMessageRequest {
                    command_id: "steer-idle-rejected".into(),
                    text: "unsupported".into(),
                    delivery: Some(DeliveryMode::Immediate),
                    ..Default::default()
                },
                client
                    .session(&idle_session.session_id)
                    .await
                    .unwrap()
                    .settings,
                Some(("unsupported_capability", "idle fallback rejected")),
            )
            .await;
        assert!(matches!(
            idle_rejected,
            Err(StoreError::Preflight("unsupported_capability", _))
        ));
        let resumed = client.claim_next_turn().await.unwrap().unwrap();
        assert!(
            resumed
                .messages
                .iter()
                .any(|message| message.text == "change direction")
        );
        assert_eq!(resumed.turn_id, work.turn_id);
        assert_eq!(resumed.initial_usage.input_tokens, Some(13));
        assert_eq!(resumed.initial_usage.output_tokens, Some(2));
        assert_eq!(resumed.initial_usage.total_tokens, Some(15));
        client
            .finish_chat_turn(
                &resumed.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: Some(resumed.requested_model.clone()),
                    usage: resumed.initial_usage,
                    status: slop_runtime::chat::ChatStatus::Interrupted,
                    error_code: Some("steering_resume".into()),
                    error_message: None,
                },
            )
            .await
            .unwrap();
        let resumed_again = client.claim_next_turn().await.unwrap().unwrap();
        assert!(
            resumed_again
                .messages
                .iter()
                .any(|message| message.text == "change direction")
        );
        assert!(
            resumed_again
                .messages
                .iter()
                .any(|message| message.text == "second direction")
        );
    }

    #[tokio::test]
    async fn pause_resume_is_durable_and_keeps_the_workspace_request_cap() {
        use slop_protocol::execution::WorkspacePolicy;
        let dir = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let mut request = create_request("pause-create", None);
        request.execution = Some(WorkspacePolicy {
            root: workspace.path().to_string_lossy().into_owned(),
            allowed_tools: vec!["read".into()],
            shell_timeout_ms: 1000,
            max_output_bytes: 1024,
            max_tool_calls: 4,
            max_model_requests: 2,
        });
        let created = client.create_session(request).await.unwrap();
        let receipt = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "pause-turn-message".into(),
                    text: "do work".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        client
            .pause_turn(
                work.turn_id.as_str(),
                TurnControlRequest {
                    command_id: "pause-command".into(),
                },
            )
            .await
            .unwrap();
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: Some(work.requested_model.clone()),
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Interrupted,
                    error_code: Some("pause_turn".into()),
                    error_message: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(client.turn(&work.turn_id).await.unwrap().status, "paused");
        assert!(client.claim_next_turn().await.unwrap().is_none());
        let paused_instruction = client
            .send_message_preflighted(
                &created.session_id,
                SendMessageRequest {
                    command_id: "paused-steering".into(),
                    text: "resume with this".into(),
                    delivery: Some(DeliveryMode::NextBoundary),
                    ..Default::default()
                },
                client.session(&created.session_id).await.unwrap().settings,
                Some(("unsupported_capability", "unused while paused")),
            )
            .await
            .unwrap();
        assert_eq!(
            paused_instruction.turn_id.as_deref(),
            Some(work.turn_id.as_str())
        );
        client
            .resume_turn(
                &work.turn_id,
                TurnControlRequest {
                    command_id: "resume-command".into(),
                },
            )
            .await
            .unwrap();
        let resumed = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(resumed.turn_id, receipt.turn_id.unwrap());
        assert_eq!(resumed.max_model_requests, 2);
        assert!(
            resumed
                .messages
                .iter()
                .any(|message| message.text == "resume with this")
        );
    }

    #[tokio::test]
    async fn canceling_paused_continuation_rejects_accepted_instructions() {
        let dir = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("cancel-paused-create", None))
            .await
            .unwrap();
        let receipt = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cancel-paused-turn".into(),
                    text: "start".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        client
            .pause_turn(
                &work.turn_id,
                TurnControlRequest {
                    command_id: "cancel-paused-pause".into(),
                },
            )
            .await
            .unwrap();
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: Some(work.requested_model),
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Interrupted,
                    error_code: Some("pause_turn".into()),
                    error_message: None,
                },
            )
            .await
            .unwrap();
        client
            .send_message_preflighted(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cancel-paused-instruction".into(),
                    text: "not applied".into(),
                    delivery: Some(DeliveryMode::Immediate),
                    ..Default::default()
                },
                client.session(&created.session_id).await.unwrap().settings,
                None,
            )
            .await
            .unwrap();
        client
            .cancel_turn(
                &work.turn_id,
                CancelTurnRequest {
                    command_id: "cancel-paused-cmd".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            client.turn(&work.turn_id).await.unwrap().status,
            "cancelled"
        );
        let instructions = client
            .steering_instructions(&created.session_id, None, 10)
            .await
            .unwrap()
            .items;
        assert_eq!(instructions[0].status, "rejected");
        assert_eq!(
            instructions[0].error_code.as_deref(),
            Some("turn_cancelled")
        );
        assert_eq!(receipt.turn_id.as_deref(), Some(work.turn_id.as_str()));
    }

    #[tokio::test]
    async fn immediate_unknown_usage_resumes_without_replaying_or_fabricating_totals() {
        use slop_runtime::chat::ChatRepository;
        let dir = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("unknown-usage-create", None))
            .await
            .unwrap();
        let receipt = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "unknown-usage-turn".into(),
                    text: "start".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        let request_id = "unknown-running-request";
        client
            .begin_request(
                &work.turn_id,
                slop_runtime::agent::RequestIntent {
                    id: request_id.into(),
                    message_id: "unknown-running-message".into(),
                    ordinal: 1,
                    requested_model: work.requested_model.clone(),
                    requested_settings: work.requested_settings.clone(),
                    settings: work.settings.clone(),
                },
            )
            .await
            .unwrap();
        client
            .send_message_preflighted(
                &created.session_id,
                SendMessageRequest {
                    command_id: "unknown-immediate".into(),
                    text: "rethink".into(),
                    delivery: Some(DeliveryMode::Immediate),
                    ..Default::default()
                },
                client.session(&created.session_id).await.unwrap().settings,
                None,
            )
            .await
            .unwrap();
        client
            .fail_request(&work.turn_id, request_id, "steering_interrupted")
            .await
            .unwrap();
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: None,
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Interrupted,
                    error_code: Some("steering_resume_unknown".into()),
                    error_message: None,
                },
            )
            .await
            .unwrap();
        let requests = client
            .model_requests(&work.turn_id, None, 10)
            .await
            .unwrap()
            .items;
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].status, "interrupted");
        assert_eq!(requests[0].usage, None);
        let resumed = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(resumed.turn_id, receipt.turn_id.unwrap());
        assert_eq!(resumed.next_request_ordinal, 2);
        assert_eq!(resumed.initial_usage.input_tokens, None);
        assert_eq!(resumed.initial_usage.output_tokens, None);
        assert_eq!(resumed.initial_usage.total_tokens, None);
        assert!(
            resumed
                .messages
                .iter()
                .any(|message| message.text == "rethink")
        );
    }

    #[tokio::test]
    async fn restart_pairs_unfinished_tools_without_replaying_completed_effects() {
        use slop_runtime::{
            agent::{RequestCompletion, RequestIntent, ToolIntent},
            chat::ChatRepository,
            providers::{Usage, inference::*},
            tools::ToolOutcome,
        };
        let dir = TempDir::new().unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let session = client
            .create_session(create_request("tool-recovery", None))
            .await
            .unwrap();
        let turn = client
            .send_message(
                &session.session_id,
                SendMessageRequest {
                    command_id: "tool-turn".into(),
                    text: "do work".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .turn_id
            .unwrap();
        client.claim_next_turn().await.unwrap().unwrap();
        client
            .begin_request(
                &turn,
                RequestIntent {
                    id: "request".into(),
                    message_id: "assistant".into(),
                    ordinal: 1,
                    requested_model: "opencode-go/glm-5.3-flash".into(),
                    requested_settings: Default::default(),
                    settings: GenerationSettings {
                        max_output_tokens: Some(1024),
                        reasoning_effort: None,
                    },
                },
            )
            .await
            .unwrap();
        let intents: Vec<_> = (0..3)
            .map(|i| ToolIntent {
                id: format!("invocation-{i}"),
                request_id: "request".into(),
                result_message_id: format!("result-{i}"),
                call_id: format!("call-{i}"),
                provider_call_id: format!("provider-{i}"),
                name: "read".into(),
                arguments: serde_json::json!({"path":"file"}),
            })
            .collect();
        let blocks = intents
            .iter()
            .enumerate()
            .map(|(i, intent)| ContentBlock {
                id: format!("block-{i}"),
                content: BlockContent::ToolCall {
                    call_id: intent.call_id.clone(),
                    provider_call_id: intent.provider_call_id.clone(),
                    name: intent.name.clone(),
                    arguments: intent.arguments.clone(),
                },
            })
            .collect();
        client.complete_request(&turn, RequestCompletion { request_id: "request".into(),
            response: InferenceResponse { resolved_model: None, usage: Usage::default(), finish_reason: FinishReason::ToolCalls,
                message: InferenceMessage { role: MessageRole::Assistant, blocks, continuation: Some(Continuation {
                    scope:"scope".into(), provider:"opencode-go".into(), model:"glm-5.3-flash".into(), wire:"chat_completions".into(), required:true,
                    items:vec![serde_json::json!({"reasoning_content":"private test continuation"})],
                }) },
            }, tools: intents.clone(),
        }).await.unwrap();
        assert_eq!(
            client
                .tool_invocations(&turn, None, 10)
                .await
                .unwrap()
                .items
                .len(),
            3
        );
        client.start_tool(&turn, &intents[0]).await.unwrap();
        let outcome = ToolOutcome {
            status: "completed".into(),
            output: "known result".into(),
            artifacts: vec![],
            error_code: None,
            effects_unknown: false,
        };
        client
            .finish_tool(&turn, &intents[0], outcome.clone())
            .await
            .unwrap();
        client
            .finish_tool(&turn, &intents[0], outcome)
            .await
            .unwrap(); // receipt retry, never execution
        client.start_tool(&turn, &intents[1]).await.unwrap();
        store.shutdown().await.unwrap();
        let store = open_test(&dir, None).await;
        let client = store.client();
        let tools = client
            .tool_invocations(&turn, None, 10)
            .await
            .unwrap()
            .items;
        assert_eq!(tools[0].status, "completed");
        assert!(!tools[0].effects_unknown);
        assert_eq!(tools[1].error_code.as_deref(), Some("daemon_restarted"));
        assert!(tools[1].effects_unknown);
        assert!(!tools[2].effects_unknown);
        assert_eq!(client.turn(&turn).await.unwrap().status, "interrupted");
        let (has_non_text, required) = client
            .context_requirements(&session.session_id)
            .await
            .unwrap();
        assert!(has_non_text);
        assert_eq!(
            required,
            vec![(
                "opencode-go".into(),
                "glm-5.3-flash".into(),
                "chat_completions".into(),
            )]
        );
        let history = client
            .messages(&session.session_id, None, 20)
            .await
            .unwrap();
        assert_eq!(history.items.len(), 5); // user, assistant, three paired results
        let public = serde_json::to_string(&history).unwrap();
        assert!(!public.contains("private test continuation"));
        assert!(!public.contains("provider-0"));
        assert!(client.claim_next_turn().await.unwrap().is_none());
        client
            .send_message(
                &session.session_id,
                SendMessageRequest {
                    command_id: "inspect".into(),
                    text: "inspect results".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let next = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(next.history.len(), 6);
        assert!(next.history.iter().any(|m| m.continuation.is_some()));
        let events = client.events(&session.session_id, 0, 200).await.unwrap();
        let count = events
            .items
            .iter()
            .filter(|e| e.kind == "tool_failed")
            .count();
        assert_eq!(count, 2);
        store.shutdown().await.unwrap();
        let store = open_test(&dir, None).await;
        let events = store
            .client()
            .events(&session.session_id, 0, 200)
            .await
            .unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == "tool_failed")
                .count(),
            count
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn identity_persists_and_name_override_is_transactional() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, Some("first")).await;
        let first = store.client().node().await.expect("identity");
        assert_eq!(first.name, "first");
        store.shutdown().await.expect("shutdown");

        let store = open_test(&dir, None).await;
        let persisted = store.client().node().await.expect("identity");
        assert_eq!(persisted.node_id, first.node_id);
        assert_eq!(persisted.name, "first");
        store.shutdown().await.expect("shutdown");

        let store = open_test(&dir, Some("second")).await;
        let changed = store.client().node().await.expect("identity");
        assert_eq!(changed.node_id, first.node_id);
        assert_eq!(changed.name, "second");
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn chat_commands_are_atomic_and_idempotent_with_payload_conflicts_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("create-1", Some("first")))
            .await
            .expect("create");
        let retry = client
            .create_session(create_request("create-1", Some("first")))
            .await
            .expect("deduplicated create");
        assert_eq!(created, retry);
        let conflict = client
            .create_session(create_request("create-1", Some("different")))
            .await;
        assert!(matches!(conflict, Err(StoreError::Conflict)));
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-1".into(),
                    text: "hello".into(),
                    expected_revision: Some(created.revision),

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let repeated = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-1".into(),
                    text: "hello".into(),
                    expected_revision: Some(created.revision),

                    ..Default::default()
                },
            )
            .await
            .expect("deduplicated send");
        assert_eq!(sent, repeated);
        assert!(matches!(
            client
                .send_message(
                    &created.session_id,
                    SendMessageRequest {
                        command_id: "send-1".into(),
                        text: "changed".into(),
                        expected_revision: Some(created.revision),
                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            client
                .messages(&created.session_id, None, 50)
                .await
                .expect("messages")
                .items
                .len(),
            1
        );
        let events = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events");
        assert_eq!(events.items.len(), 2);
        assert_eq!(events.items[1].sequence, sent.event_sequence);
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn claim_context_is_ordered_and_excludes_later_queued_users() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("create-2", None))
            .await
            .expect("create");
        let first = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-2a".into(),
                    text: "first question".into(),
                    expected_revision: Some(created.revision),

                    ..Default::default()
                },
            )
            .await
            .expect("first");
        let second = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-2b".into(),
                    text: "later queued question".into(),
                    expected_revision: Some(first.revision),

                    ..Default::default()
                },
            )
            .await
            .expect("second");
        let work = client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("turn");
        assert_eq!(work.turn_id, first.turn_id.clone().expect("turn id"));
        assert_eq!(work.requested_model, "opencode-go/glm-5.3-flash");
        assert_eq!(work.max_tokens, Some(1024));
        assert_eq!(work.messages.len(), 1);
        assert_eq!(work.messages[0].text, "first question");
        assert!(
            client
                .claim_next_turn()
                .await
                .expect("claim while active")
                .is_none()
        );
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "answer".into(),
                    resolved_model: Some(work.requested_model.clone()),
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("finish");
        let next = client
            .claim_next_turn()
            .await
            .expect("next claim")
            .expect("second turn");
        assert_eq!(next.turn_id, second.turn_id.expect("turn id"));
        let context: Vec<String> = next
            .messages
            .into_iter()
            .map(|message| message.text)
            .collect();
        assert_eq!(
            context,
            ["first question", "answer", "later queued question"]
        );
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn oversized_context_is_failed_before_a_provider_claim() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("context-create", None))
            .await
            .expect("create");
        for index in 0..128 {
            let sent = client
                .send_message(
                    &created.session_id,
                    SendMessageRequest {
                        command_id: format!("context-{index}"),
                        text: "question".into(),
                        expected_revision: None,

                        ..Default::default()
                    },
                )
                .await
                .expect("send");
            let work = client
                .claim_next_turn()
                .await
                .expect("claim")
                .expect("work");
            assert_eq!(work.turn_id, sent.turn_id.expect("turn id"));
            client
                .finish_chat_turn(
                    &work.turn_id,
                    slop_runtime::chat::ChatOutcome {
                        text: "answer".into(),
                        resolved_model: None,
                        usage: slop_runtime::providers::Usage::default(),
                        status: slop_runtime::chat::ChatStatus::Completed,
                        error_code: None,
                        error_message: None,
                    },
                )
                .await
                .expect("finish");
        }
        let pending = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "context-overflow".into(),
                    text: "last question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("queue pending");
        assert!(
            client
                .claim_next_turn()
                .await
                .expect("bounded claim")
                .is_none()
        );
        let turn = client
            .turn(&pending.turn_id.expect("turn id"))
            .await
            .expect("failed turn");
        assert_eq!(turn.status, "failed");
        assert_eq!(turn.error_code.as_deref(), Some("context_limit"));
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn startup_recovery_keeps_visible_checkpoint_out_of_context_and_marks_event() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("create-3", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-3".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        client
            .checkpoint_visible(&turn_id, "visible partial")
            .await
            .expect("checkpoint");
        store.shutdown().await.expect("shutdown");
        let store = open_test(&dir, None).await;
        let client = store.client();
        assert_eq!(
            client.turn(&turn_id).await.expect("turn").status,
            "interrupted"
        );
        let messages = client
            .messages(&created.session_id, None, 50)
            .await
            .expect("messages")
            .items;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].text, "visible partial");
        assert_eq!(messages[1].status, "interrupted");
        let events = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events");
        assert_eq!(
            events.items.last().expect("terminal event").kind,
            "turn_interrupted"
        );
        let session = client.session(&created.session_id).await.expect("session");
        let next = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-3b".into(),
                    text: "new question".into(),
                    expected_revision: Some(session.revision),

                    ..Default::default()
                },
            )
            .await
            .expect("next message");
        let resumed = client
            .claim_next_turn()
            .await
            .expect("claim after recovery")
            .expect("queued turn");
        assert_eq!(resumed.turn_id, next.turn_id.expect("turn id"));
        assert_eq!(resumed.messages.len(), 1);
        assert_eq!(resumed.messages[0].text, "new question");
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn completed_empty_output_replaces_a_nonempty_checkpoint() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("empty-create", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "empty-send".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        client
            .checkpoint_visible(&turn_id, "partial text")
            .await
            .expect("checkpoint");
        client
            .finish_chat_turn(
                &turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: String::new(),
                    resolved_model: None,
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("finish");
        let messages = client
            .messages(&created.session_id, None, 50)
            .await
            .expect("history")
            .items;
        assert_eq!(messages[1].text, "");
        assert_eq!(messages[1].status, "completed");
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn usage_counts_and_provenance_survive_restart() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("usage-create", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "usage-send".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        let work = client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        client
            .finish_chat_turn(
                &turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "answer".into(),
                    resolved_model: Some(work.requested_model),
                    usage: slop_runtime::providers::Usage::from_reported(
                        Some(u64::MAX),
                        Some(7),
                        Some(u64::MAX),
                    ),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("finish");
        store.shutdown().await.expect("shutdown");

        let store = open_test(&dir, None).await;
        let turn = store.client().turn(&turn_id).await.expect("persisted turn");
        let usage = turn.usage.expect("usage");
        assert_eq!(usage.input_tokens, Some(u64::MAX));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(usage.total_tokens, Some(u64::MAX));
        assert_eq!(usage.total_source.as_deref(), Some("reported"));
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn session_pages_use_stable_creation_order_cursors() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let a = client
            .create_session(create_request("page-a", None))
            .await
            .expect("a");
        let b = client
            .create_session(create_request("page-b", None))
            .await
            .expect("b");
        let c = client
            .create_session(create_request("page-c", None))
            .await
            .expect("c");
        let first = client.sessions(None, 2).await.expect("first page");
        assert_eq!(
            first
                .items
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            [a.session_id.as_str(), b.session_id.as_str()]
        );
        let cursor = first.next_after.expect("next cursor");
        let second = client.sessions(Some(cursor), 2).await.expect("second page");
        assert_eq!(
            second
                .items
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            [c.session_id.as_str()]
        );
        assert_eq!(second.next_after, None);
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn configured_default_is_persisted_without_changing_retry_payload() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let mut request = create_request("default-cap", None);
        request.max_tokens = None;
        let receipt = client
            .create_session_with_default_max_tokens(request.clone(), Some(12345))
            .await
            .expect("create");
        let retry = client
            .create_session_with_default_max_tokens(request, Some(54321))
            .await
            .expect("retry with changed daemon config");
        assert_eq!(receipt, retry);
        let session = client.session(&receipt.session_id).await.expect("session");
        assert_eq!(session.max_tokens, Some(12345));
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn queued_cancel_is_terminal_and_idempotent() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("cancel-queued-create", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cancel-queued-send".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        let request = CancelTurnRequest {
            command_id: "cancel-queued".into(),
        };
        let receipt = client
            .cancel_turn(&turn_id, request.clone())
            .await
            .expect("cancel queued");
        let retry = client
            .cancel_turn(&turn_id, request)
            .await
            .expect("cancel retry");
        assert_eq!(receipt, retry);
        assert_eq!(
            client.turn(&turn_id).await.expect("turn").status,
            "cancelled"
        );
        assert!(client.claim_next_turn().await.expect("claim").is_none());
        let events = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events");
        assert_eq!(
            events.items.last().expect("cancel event").kind,
            "turn_cancelled"
        );
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn accepted_running_cancel_overrides_later_completion_and_retries_original_receipt() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("cancel-running-create", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cancel-running-send".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        let work = client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        let request = CancelTurnRequest {
            command_id: "cancel-running".into(),
        };
        let receipt = client
            .cancel_turn(&turn_id, request.clone())
            .await
            .expect("request cancellation");
        client
            .finish_chat_turn(
                &turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "late completion".into(),
                    resolved_model: Some(work.requested_model),
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("cancellation wins");
        assert_eq!(
            client.turn(&turn_id).await.expect("turn").status,
            "cancelled"
        );
        assert_eq!(
            client
                .cancel_turn(&turn_id, request)
                .await
                .expect("retry after terminal state"),
            receipt
        );
        let events = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events");
        assert_eq!(
            events.items.last().expect("terminal event").kind,
            "turn_cancelled"
        );
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn cancel_after_completion_conflicts_without_appending_event() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("cancel-done-create", None))
            .await
            .expect("create");
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cancel-done-send".into(),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let turn_id = sent.turn_id.expect("turn id");
        let work = client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        client
            .finish_chat_turn(
                &turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "complete".into(),
                    resolved_model: Some(work.requested_model),
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("finish");
        let before = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events before");
        assert!(matches!(
            client
                .cancel_turn(
                    &turn_id,
                    CancelTurnRequest {
                        command_id: "cancel-too-late".into(),
                    }
                )
                .await,
            Err(StoreError::Conflict)
        ));
        let after = client
            .events(&created.session_id, 0, 50)
            .await
            .expect("events after");
        assert_eq!(before, after);
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn command_id_reuse_across_operations_conflicts_without_mutation() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("cross-operation", None))
            .await
            .expect("create");
        assert!(matches!(
            client
                .send_message(
                    &created.session_id,
                    SendMessageRequest {
                        command_id: "cross-operation".into(),
                        text: "must not be inserted".into(),
                        expected_revision: None,

                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            client
                .messages(&created.session_id, None, 50)
                .await
                .expect("empty history")
                .items
                .is_empty()
        );
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn message_pages_bound_escaped_json_and_resume_without_loss() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("escaped-page-create", None))
            .await
            .expect("create");
        store.shutdown().await.expect("close before fixture");

        let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
        for index in 1..=2 {
            let turn_id = format!("fixture-turn-{index}");
            let user_id = format!("fixture-user-{index}");
            let assistant_id = format!("fixture-assistant-{index}");
            conn.execute(
                "INSERT INTO turns(id,session_id,ordinal,user_message_id,status,requested_model) VALUES(?1,?2,?3,?4,'completed','opencode-go/glm-5.3-flash')",
                rusqlite::params![turn_id,created.session_id,index,user_id],
            )
            .expect("fixture turn");
            let escaped_text = "\u{0001}".repeat(MAX_CONTEXT_BYTES);
            conn.execute(
                "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'assistant',?5,'completed')",
                rusqlite::params![assistant_id,created.session_id,turn_id,index*2+1,escaped_text],
            )
            .expect("fixture message");
        }
        drop(conn);

        let store = open_test(&dir, None).await;
        let client = store.client();
        let first = client
            .messages(&created.session_id, None, 200)
            .await
            .expect("bounded first page");
        assert_eq!(first.items.len(), 1);
        assert!(first.next_after.is_some());
        assert!(serde_json::to_vec(&first).expect("encode page").len() <= MAX_MESSAGE_PAGE_BYTES);
        let second = client
            .messages(&created.session_id, first.next_after, 200)
            .await
            .expect("second page");
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.next_after, None);
        assert!(serde_json::to_vec(&second).expect("encode page").len() <= MAX_MESSAGE_PAGE_BYTES);
        assert_eq!(first.items[0].text, second.items[0].text);
        assert_eq!(first.items[0].text.len(), MAX_CONTEXT_BYTES);
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn send_event_transaction_failure_rolls_back_turn_message_and_receipt() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let client = store.client();
        let created = client
            .create_session(create_request("rollback-create", None))
            .await
            .expect("create");
        store
            .shutdown()
            .await
            .expect("close before fault injection");

        let path = dir.path().join(DATABASE_FILE);
        let conn = Connection::open(&path).expect("db");
        conn.execute_batch(
            "CREATE TRIGGER fail_user_event BEFORE INSERT ON session_events WHEN NEW.kind='user_message_accepted' BEGIN SELECT RAISE(ABORT,'injected event failure'); END;",
        )
        .expect("install trigger");
        drop(conn);

        let store = open_test(&dir, None).await;
        let client = store.client();
        let request = SendMessageRequest {
            command_id: "rollback-send".into(),
            text: "atomic message".into(),
            expected_revision: None,

            ..Default::default()
        };
        assert!(matches!(
            client
                .send_message(&created.session_id, request.clone())
                .await,
            Err(StoreError::Database)
        ));
        store
            .shutdown()
            .await
            .expect("shutdown after injected failure");

        let conn = Connection::open(&path).expect("db");
        let turns: i64 = conn
            .query_row(
                "SELECT count(*) FROM turns WHERE session_id=?1",
                [&created.session_id],
                |row| row.get(0),
            )
            .expect("turn count");
        let messages: i64 = conn
            .query_row(
                "SELECT count(*) FROM messages WHERE session_id=?1",
                [&created.session_id],
                |row| row.get(0),
            )
            .expect("message count");
        let commands: i64 = conn
            .query_row(
                "SELECT count(*) FROM commands WHERE command_id='rollback-send'",
                [],
                |row| row.get(0),
            )
            .expect("receipt count");
        assert_eq!((turns, messages, commands), (0, 0, 0));
        conn.execute_batch("DROP TRIGGER fail_user_event;")
            .expect("remove trigger");
        drop(conn);

        let store = open_test(&dir, None).await;
        let receipt = store
            .client()
            .send_message(&created.session_id, request)
            .await
            .expect("retry after rollback");
        assert!(receipt.turn_id.is_some());
        assert_eq!(
            store
                .client()
                .messages(&created.session_id, None, 50)
                .await
                .expect("committed message")
                .items
                .len(),
            1
        );
        store.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn second_store_owner_is_rejected_and_shutdown_releases_lock() {
        let dir = TempDir::new().expect("temp dir");
        let store = open_test(&dir, None).await;
        let err = Store::open(
            dir.path().to_path_buf(),
            None,
            2,
            Duration::from_millis(100),
        )
        .await
        .err()
        .expect("only one owner");
        assert!(matches!(err, StoreError::AlreadyRunning));
        store.shutdown().await.expect("shutdown");
        let reopened = open_test(&dir, None).await;
        reopened.shutdown().await.expect("shutdown");
    }

    #[tokio::test]
    async fn bounded_queue_reports_busy_and_shutdown_drains_accepted_calls() {
        let dir = TempDir::new().expect("temp dir");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
                .expect("private temp dir");
        }
        let store = Store::open(
            dir.path().to_path_buf(),
            None,
            1,
            Duration::from_millis(200),
        )
        .await
        .expect("store opens");
        let client = store.client();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        client
            .shared
            .sender
            .try_send(Request::Hold {
                started: started_tx,
                release: release_rx,
            })
            .expect("worker hold accepted");
        started_rx.await.expect("worker reached hold");

        let (reply_tx, reply_rx) = oneshot::channel();
        client
            .shared
            .sender
            .try_send(Request::Node(reply_tx))
            .expect("one request fits bounded queue");
        assert!(matches!(client.node().await, Err(StoreError::Busy)));

        let shutdown = tokio::spawn(store.shutdown());
        tokio::task::yield_now().await;
        release_tx.send(()).expect("release worker");
        let result = reply_rx
            .await
            .expect("accepted request replied")
            .expect("node");
        assert!(!result.node_id.is_empty());
        shutdown
            .await
            .expect("shutdown task")
            .expect("clean shutdown");
        assert!(matches!(client.node().await, Err(StoreError::Unavailable)));
    }

    #[tokio::test]
    async fn dropping_shutdown_future_keeps_lock_until_worker_closes() {
        let dir = TempDir::new().expect("temp dir");
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
                .expect("private temp dir");
        }
        let store = Store::open(
            dir.path().to_path_buf(),
            None,
            1,
            Duration::from_millis(200),
        )
        .await
        .expect("store opens");
        let client = store.client();
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        assert!(
            client
                .shared
                .sender
                .try_send(Request::Hold {
                    started: started_tx,
                    release: release_rx,
                })
                .is_ok()
        );
        started_rx.await.expect("worker reached hold");

        let (reply_tx, reply_rx) = oneshot::channel();
        assert!(
            client
                .shared
                .sender
                .try_send(Request::Node(reply_tx))
                .is_ok()
        );
        let shutdown = tokio::spawn(store.shutdown());
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(client.node().await, Err(StoreError::Unavailable)) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shutdown started");
        shutdown.abort();
        let _ = shutdown.await;

        let err = Store::open(
            dir.path().to_path_buf(),
            None,
            2,
            Duration::from_millis(100),
        )
        .await
        .err()
        .expect("worker still owns lock");
        assert!(matches!(err, StoreError::AlreadyRunning));

        release_tx.send(()).expect("release worker");
        reply_rx
            .await
            .expect("accepted request completed")
            .expect("identity query");
        let reopened = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match Store::open(
                    dir.path().to_path_buf(),
                    None,
                    2,
                    Duration::from_millis(100),
                )
                .await
                {
                    Ok(store) => break store,
                    Err(StoreError::AlreadyRunning) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("reopen failed: {error}"),
                }
            }
        })
        .await
        .expect("worker eventually released lock");
        reopened.shutdown().await.expect("shutdown");
    }

    #[test]
    fn newer_schema_is_rejected_before_journal_mode_changes() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE);
        let conn = Connection::open(&path).expect("db");
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .expect("version");
        drop(conn);
        let before = fs::read(&path).expect("database bytes");
        let result = open_and_initialize(dir.path(), None, Duration::from_millis(100));
        assert!(matches!(result, Err(StoreError::NewerSchema { .. })));
        let after = fs::read(&path).expect("database bytes");
        assert_eq!(before, after);
        assert!(!dir.path().join(format!("{DATABASE_FILE}-wal")).exists());
        assert!(!dir.path().join(format!("{DATABASE_FILE}-shm")).exists());
    }

    #[test]
    fn negative_schema_version_is_rejected_without_mutation() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE);
        let conn = Connection::open(&path).expect("db");
        conn.pragma_update(None, "user_version", -1)
            .expect("version");
        drop(conn);
        let before = fs::read(&path).expect("database bytes");
        assert!(matches!(
            open_and_initialize(dir.path(), None, Duration::from_millis(100)),
            Err(StoreError::Database)
        ));
        assert_eq!(fs::read(&path).expect("database bytes"), before);
        assert!(!dir.path().join(format!("{DATABASE_FILE}-wal")).exists());
    }

    #[test]
    fn invalid_existing_identity_is_not_modified_by_name_override() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE);
        let conn = Connection::open(&path).expect("db");
        migrate(&conn, 0, Some("original".to_owned())).expect("migration");
        conn.execute("UPDATE node_identity SET node_id = 'invalid'", [])
            .expect("invalid fixture");
        drop(conn);

        assert!(matches!(
            open_and_initialize(
                dir.path(),
                Some("replacement".to_owned()),
                Duration::from_millis(100)
            ),
            Err(StoreError::Database)
        ));
        let conn = Connection::open(path).expect("db");
        let name: String = conn
            .query_row(
                "SELECT name FROM node_identity WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("identity name");
        assert_eq!(name, "original");
    }

    #[test]
    fn migration_failure_rolls_back_identity_and_schema() {
        let dir = TempDir::new().expect("temp dir");
        let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
        let result = migrate_with_hook(&conn, 0, Some("partial".to_owned()), |tx| {
            tx.execute(
                "INSERT INTO node_identity VALUES (1, 'injected', 'partial', 'test')",
                [],
            )
            .map_err(|_| StoreError::Database)?;
            Err(StoreError::Database)
        });
        assert!(result.is_err());
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_identity'",
                [],
                |row| row.get(0),
            )
            .expect("table count");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(tables, 0);
        assert_eq!(version, 0);
    }

    #[test]
    fn corrupt_existing_database_is_not_recreated() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE);
        fs::write(&path, b"not sqlite").expect("corrupt bytes");
        let before = fs::read(&path).expect("before");
        assert!(open_and_initialize(dir.path(), None, Duration::from_millis(100)).is_err());
        assert_eq!(fs::read(&path).expect("after"), before);
    }

    #[cfg(unix)]
    #[test]
    fn database_and_lock_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("other-file");
        fs::write(&target, b"keep").expect("target");
        symlink(&target, dir.path().join(DATABASE_FILE)).expect("database symlink");
        assert!(matches!(
            open_and_initialize(dir.path(), None, Duration::from_millis(100)),
            Err(StoreError::Configuration)
        ));
        fs::remove_file(dir.path().join(DATABASE_FILE)).expect("remove db link");
        symlink(&target, dir.path().join(LOCK_FILE)).expect("lock symlink");
        assert!(matches!(
            open_lock(dir.path()),
            Err(StoreError::Configuration)
        ));
        assert_eq!(fs::read(target).expect("target remains"), b"keep");
    }

    #[test]
    fn version_zero_database_with_unrecognized_schema_is_rejected() {
        let dir = TempDir::new().expect("temp dir");
        let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
        conn.execute("CREATE TABLE unrelated (value TEXT)", [])
            .expect("unrecognized schema");
        drop(conn);
        assert!(matches!(
            open_and_initialize(dir.path(), None, Duration::from_millis(100)),
            Err(StoreError::Database)
        ));
    }

    #[test]
    fn migration_wrapper_uses_one_transaction() {
        let dir = TempDir::new().expect("temp dir");
        let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
        migrate(&conn, 0, None).expect("migration");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn schema_one_migration_preserves_node_identity_and_adds_chat_tables() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join(DATABASE_FILE);
        let conn = Connection::open(&path).expect("db");
        conn.execute_batch(
            "CREATE TABLE node_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1),node_id TEXT NOT NULL UNIQUE,name TEXT NOT NULL,os TEXT NOT NULL);
             INSERT INTO node_identity VALUES(1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','legacy','linux');
             PRAGMA user_version=1;",
        )
        .expect("schema one fixture");
        drop(conn);

        let conn = open_and_initialize(dir.path(), None, Duration::from_millis(100))
            .expect("migrate schema one");
        let identity = query_node(&conn).expect("identity");
        assert_eq!(identity.node_id, "a".repeat(64));
        assert_eq!(identity.name, "legacy");
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions','turns','messages','session_events','commands')",
                [],
                |row| row.get(0),
            )
            .expect("chat tables");
        assert_eq!(tables, 5);
    }

    #[test]
    fn initialized_database_has_required_pragmas_and_supported_sqlite() {
        let dir = TempDir::new().expect("temp dir");
        let conn = open_and_initialize(dir.path(), None, Duration::from_millis(137))
            .expect("initialized db");
        verify_pragmas(&conn, Duration::from_millis(137)).expect("pragmas");
        assert!(rusqlite::version_number() >= MINIMUM_SQLITE_VERSION);
    }

    fn migrate_with_hook<F>(
        connection: &Connection,
        existing_version: i64,
        node_name: Option<String>,
        hook: F,
    ) -> StoreResult<()>
    where
        F: FnOnce(&Transaction<'_>) -> StoreResult<()>,
    {
        super::migrate_with_hook(connection, existing_version, node_name, hook)
    }
}
