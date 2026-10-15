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

mod children;
mod context;
mod conversations;
mod execution;
mod governance;
mod history;
mod inbox;
pub(crate) mod orchestration;
mod peers;
mod projects;

const DATABASE_FILE: &str = "state.sqlite3";
const LOCK_FILE: &str = "daemon.lock";
const SCHEMA_VERSION: i64 = 21;
const MESSAGE_ORDER_STRIDE: i64 = 1 << 32;
const MINIMUM_SQLITE_VERSION: i32 = 3_051_003;
const MAX_QUEUE_CAPACITY: usize = 4096;
const MAX_BUSY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_NODE_NAME_BYTES: usize = 128;
const MAX_COMMAND_ID_BYTES: usize = 128;
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_CONTEXT_BYTES: usize = slop_runtime::providers::ChatRequest::MAX_REQUEST_BYTES;
const MAX_CONTEXT_MESSAGES: usize = slop_runtime::providers::ChatRequest::MAX_MESSAGES;
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
    #[error("stop outstanding turns before deleting conversation history")]
    HistoryBusy,
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
    context_token_budget: std::sync::atomic::AtomicU64,
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

mod lifecycle;
#[cfg(test)]
use lifecycle::open_and_initialize;
#[cfg(all(test, unix))]
use lifecycle::open_lock;
#[cfg(test)]
mod long_turn_tests;
mod messages;
mod repository;
mod schema;
mod sessions;
mod support;
#[cfg(test)]
mod tests;
mod turns;

use messages::*;
use schema::*;
use sessions::*;
use support::*;
use turns::*;
