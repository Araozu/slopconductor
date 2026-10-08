//! Daemon-owned SQLite identity store.
//!
//! The store intentionally contains only node identity. Session and execution
//! records will arrive with the behavior that owns their recovery semantics.

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
use slop_protocol::NodeResponse;
use tokio::sync::oneshot;

const DATABASE_FILE: &str = "state.sqlite3";
const LOCK_FILE: &str = "daemon.lock";
const SCHEMA_VERSION: i64 = 1;
const MINIMUM_SQLITE_VERSION: i32 = 3_051_003;
const MAX_QUEUE_CAPACITY: usize = 4096;
const MAX_BUSY_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_NODE_NAME_BYTES: usize = 128;
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
}

type StoreResult<T> = Result<T, StoreError>;

struct Gate {
    accepting: bool,
}

struct Shared {
    sender: SyncSender<Request>,
    gate: Arc<Mutex<Gate>>,
    alive: Arc<AtomicBool>,
}

enum Request {
    Node(oneshot::Sender<StoreResult<NodeResponse>>),
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
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
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
    hook(&tx)?;
    tx.commit().map_err(|_| StoreError::Database)
}

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
