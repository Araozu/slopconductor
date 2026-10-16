use super::*;

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
            context_token_budget: std::sync::atomic::AtomicU64::new(
                slop_runtime::agent::context::DEFAULT_TOKEN_BUDGET,
            ),
        });
        let (ready_tx, ready_rx) = oneshot::channel();
        let (finished_tx, finished_rx) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("hive-db".to_owned())
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
    /// Inspect the private continuation requirements in bounded durable history.
    /// Private continuation items remain inside the storage/runtime boundary.
    #[cfg(test)]
    pub async fn context_requirements(
        &self,
        session_id: &str,
    ) -> StoreResult<(bool, Vec<(String, String, String)>)> {
        let session_id = session_id.to_owned();
        self.submit(move |connection| {
            let (through, saved) = context::load(connection, &session_id)?;
            let mut non_text = false;
            let mut required = Vec::new();
            for message in saved {
                non_text |= message.blocks.iter().any(|block| !matches!(block.content, slop_runtime::providers::inference::BlockContent::Text { .. }));
                if let Some(c) = message.continuation.filter(|c| c.required) { required.push((c.provider,c.model,c.wire)); }
            }
            let mut statement = connection.prepare(
                "SELECT m.blocks,m.continuation FROM messages m JOIN turns t ON t.id=m.turn_id
                 WHERE m.session_id=?1 AND m.deleted=0 AND m.context_excluded=0 AND m.ordinal>?2 AND m.status='completed' AND NOT EXISTS(SELECT 1 FROM model_requests r WHERE r.id=m.request_id AND json_extract(r.context,'$.compaction')=1)
                   AND (t.status IN('completed','failed','cancelled','interrupted','incomplete')
                     OR (t.status='running' AND EXISTS(SELECT 1 FROM tool_invocations i WHERE i.turn_id=t.id)))
                 ORDER BY m.ordinal LIMIT ?3",
            ).map_err(|_| StoreError::Database)?;
            let mut rows = statement.query(rusqlite::params![session_id,through,MAX_CONTEXT_MESSAGES as i64]).map_err(|_| StoreError::Database)?;
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
        let resolved = self.resolve_workspace(&request).await?;
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

    pub async fn rename_session(
        &self,
        id: &str,
        request: slop_protocol::chat::RenameSessionRequest,
    ) -> StoreResult<CommandReceipt> {
        let id = id.to_owned();
        self.submit(move |connection| rename_session(connection, &id, request))
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
        let budget = self.shared.context_token_budget.load(Ordering::Acquire);
        self.submit(move |c| {
            let mut work = claim_next_turn(c)?;
            if let Some(work) = &mut work {
                work.context_token_budget = budget;
            }
            Ok(work)
        })
        .await
    }

    pub fn set_context_token_budget(&self, budget: u64) {
        self.shared
            .context_token_budget
            .store(budget, Ordering::Release);
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
                "SELECT status IN ('queued','running','awaiting_children') AND (EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='applied') OR EXISTS(SELECT 1 FROM child_waits WHERE parent_turn_id=?1) OR EXISTS(SELECT 1 FROM session_events WHERE turn_id=?1 AND kind IN('turn_context_yielded','turn_awaiting_children','child_completion_delivered'))) FROM turns WHERE id=?1",
                [&turn_id],
                |row| row.get(0),
            ).optional().map_err(|_| StoreError::Database)?.ok_or(StoreError::NotFound)?;
            Ok(queued)
        }).await
    }

    pub(super) async fn submit<T, F>(&self, operation: F) -> StoreResult<T>
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

pub(super) fn open_lock(data_dir: &Path) -> StoreResult<File> {
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

pub(super) fn open_and_initialize(
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
