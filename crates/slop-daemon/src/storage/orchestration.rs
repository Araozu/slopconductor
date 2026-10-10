//! Jobs share the existing turn scheduler and its transactional lifecycle.
use super::*;
use slop_protocol::orchestration::*;

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "CREATE TABLE batches (
        id TEXT PRIMARY KEY, owner_node_id TEXT NOT NULL, spec TEXT NOT NULL,
        max_concurrent_runs INTEGER NOT NULL CHECK(max_concurrent_runs BETWEEN 1 AND 16),
        created_order INTEGER NOT NULL UNIQUE);
        CREATE TABLE tasks (
        id TEXT PRIMARY KEY, owner_node_id TEXT NOT NULL, spec TEXT NOT NULL, requested_spec TEXT NOT NULL,
        batch_id TEXT REFERENCES batches(id), combination_index INTEGER,
        last_event_sequence INTEGER NOT NULL DEFAULT 0, created_order INTEGER NOT NULL UNIQUE,
        UNIQUE(batch_id,combination_index));
        CREATE TABLE runs (
        id TEXT PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
        attempt INTEGER NOT NULL, retry_of TEXT REFERENCES runs(id),
        session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id),
        turn_id TEXT NOT NULL UNIQUE REFERENCES turns(id), UNIQUE(task_id,attempt));
        CREATE TABLE task_events (
        task_id TEXT NOT NULL REFERENCES tasks(id), sequence INTEGER NOT NULL,
        run_id TEXT NOT NULL REFERENCES runs(id), kind TEXT NOT NULL, status TEXT NOT NULL,
        session_event_sequence INTEGER, PRIMARY KEY(task_id,sequence));
        CREATE INDEX tasks_batch ON tasks(batch_id,created_order);",
    )
    .map_err(|_| StoreError::Database)
}

fn event(
    tx: &Transaction<'_>,
    task: &str,
    run: &str,
    kind: &str,
    status: &str,
    source: Option<u64>,
) -> StoreResult<u64> {
    tx.execute(
        "UPDATE tasks SET last_event_sequence=last_event_sequence+1 WHERE id=?1",
        [task],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute(
        "INSERT INTO task_events(task_id,sequence,run_id,kind,status,session_event_sequence)
        VALUES(?1,(SELECT last_event_sequence FROM tasks WHERE id=?1),?2,?3,?4,?5)",
        rusqlite::params![task, run, kind, status, source.map(|s| s as i64)],
    )
    .map_err(|_| StoreError::Database)?;
    tx.query_row(
        "SELECT last_event_sequence FROM tasks WHERE id=?1",
        [task],
        |r| r.get::<_, i64>(0).map(|s| s as u64),
    )
    .map_err(|_| StoreError::Database)
}

pub(super) fn session_event(
    tx: &Transaction<'_>,
    session: &str,
    kind: &str,
    turn: Option<&str>,
    source: u64,
) -> StoreResult<()> {
    let row: Option<(String,String,String)> = tx.query_row(
        "SELECT r.task_id,r.id,t.status FROM runs r JOIN turns t ON t.id=r.turn_id WHERE r.session_id=?1 AND (?2 IS NULL OR r.turn_id=?2)",
        rusqlite::params![session,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|StoreError::Database)?;
    if let Some((task, run, status)) = row {
        event(tx, &task, &run, kind, &status, Some(source))?;
    }
    Ok(())
}

fn run(connection: &Connection, id: &str) -> StoreResult<RunResponse> {
    let (task_id, attempt, retry_of, session_id, turn_id): (
        String,
        u32,
        Option<String>,
        String,
        String,
    ) = connection
        .query_row(
            "SELECT task_id,attempt,retry_of,session_id,turn_id FROM runs WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    let session = get_session(connection, &session_id)?;
    let effects_unknown: bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations WHERE turn_id=?1 AND (status='running' OR json_extract(outcome,'$.effects_unknown')=1)) OR EXISTS(SELECT 1 FROM managed_workspaces WHERE session_id=?2 AND effects_unknown=1)",rusqlite::params![turn_id,session_id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
    Ok(RunResponse {
        id: id.into(),
        task_id,
        attempt,
        retry_of,
        session_id,
        workspace_id: session.managed_workspace_id,
        effects_unknown,
        turn: get_turn(connection, &turn_id)?,
    })
}

fn task(connection: &Connection, id: &str) -> StoreResult<TaskResponse> {
    let (owner,spec,batch,index,sequence):(String,String,Option<String>,Option<u32>,i64)=connection.query_row(
        "SELECT owner_node_id,spec,batch_id,combination_index,last_event_sequence FROM tasks WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)?;
    let latest: String = connection
        .query_row(
            "SELECT id FROM runs WHERE task_id=?1 ORDER BY attempt DESC LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let requested: String = connection
        .query_row("SELECT requested_spec FROM tasks WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    Ok(TaskResponse {
        id: id.into(),
        owner_node_id: owner,
        spec: serde_json::from_str(&spec).map_err(|_| StoreError::Database)?,
        requested_spec: serde_json::from_str(&requested).map_err(|_| StoreError::Database)?,
        batch_id: batch,
        combination_index: index,
        last_event_sequence: sequence as u64,
        latest_run: run(connection, &latest)?,
    })
}

pub(crate) fn validate_spec(spec: &TaskSpec) -> StoreResult<()> {
    if spec.prompt.trim().is_empty()
        || spec.prompt.len() > MAX_MESSAGE_BYTES
        || spec.prompt.contains('\0')
        || spec
            .title
            .as_ref()
            .is_some_and(|s| s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(StoreError::Invalid);
    }
    if let Some(project) = &spec.project {
        let mut names = std::collections::BTreeSet::new();
        if project.project_id.len() > 128
            || project.allowed_tools.is_empty()
            || project.allowed_tools.len() > 4
            || project.allowed_tools.iter().any(|n| {
                !matches!(n.as_str(), "read" | "write" | "edit" | "bash") || !names.insert(n)
            })
        {
            return Err(StoreError::Invalid);
        }
    }
    execution::validate_settings(&spec.model, &spec.settings)
}

pub(crate) fn combinations(spec: &BatchSpec) -> StoreResult<Vec<CombinationResponse>> {
    if spec.name.trim().is_empty()
        || spec.name.len() > 256
        || spec.name.chars().any(char::is_control)
        || spec
            .default_max_output_tokens
            .is_some_and(|n| n == 0 || n > 65_536)
        || !(1..=16).contains(&spec.max_concurrent_runs)
    {
        return Err(StoreError::Invalid);
    }
    let indices = slop_core::orchestration::matrix_indices([
        spec.prompts.len(),
        spec.models.len(),
        spec.settings.len(),
    ])
    .ok_or(StoreError::Limit)?;
    indices
        .into_iter()
        .enumerate()
        .map(|(index, [p, m, s])| {
            let task = TaskSpec {
                title: Some(spec.name.clone()),
                prompt: spec.prompts[p].clone(),
                model: spec.models[m].clone(),
                settings: spec.settings[s].clone(),
                project: spec.project.clone(),
            };
            validate_spec(&task)?;
            Ok(CombinationResponse {
                index: index as u32,
                prompt_index: p as u32,
                model_index: m as u32,
                settings_index: s as u32,
                spec: task,
            })
        })
        .collect()
}

fn insert_run(
    tx: &Transaction<'_>,
    task_id: &str,
    spec: &TaskSpec,
    resolved: Option<projects::ResolvedWorkspace>,
    previous: Option<&RunResponse>,
    command: &str,
) -> StoreResult<TaskReceipt> {
    validate_spec(spec)?;
    let active: i64 = tx.query_row("SELECT count(*) FROM runs r JOIN turns t ON t.id=r.turn_id WHERE t.status IN('queued','running','paused')", [], |r|r.get(0)).map_err(|_|StoreError::Database)?;
    if active >= MAX_QUEUE_GLOBAL {
        return Err(StoreError::Limit);
    }
    let model: slop_core::provider::ProviderModelRef =
        spec.model.parse().map_err(|_| StoreError::Invalid)?;
    let session = create_session_tx(
        tx,
        CreateSessionRequest {
            command_id: opaque_id()?,
            title: spec.title.clone(),
            provider: model.provider().as_str().into(),
            model: model.model().into(),
            max_tokens: None,
            settings: Some(spec.settings.clone()),
            execution: None,
            project: spec.project.clone(),
        },
        spec.settings.max_output_tokens,
        resolved,
    )?;
    let turn = send_message_tx(
        tx,
        &session.session_id,
        SendMessageRequest {
            command_id: opaque_id()?,
            text: spec.prompt.clone(),
            settings: Some(
                tx.query_row(
                    "SELECT requested_spec FROM tasks WHERE id=?1",
                    [task_id],
                    |r| r.get::<_, String>(0),
                )
                .map_err(|_| StoreError::Database)
                .and_then(|s| {
                    serde_json::from_str::<TaskSpec>(&s)
                        .map(|s| s.settings)
                        .map_err(|_| StoreError::Database)
                })?,
            ),
            ..Default::default()
        },
        Some(spec.settings.clone()),
        None,
    )?;
    let run_id = opaque_id()?;
    let turn_id = turn.turn_id.ok_or(StoreError::Database)?;
    tx.execute("INSERT INTO runs(id,task_id,attempt,retry_of,session_id,turn_id) VALUES(?1,?2,?3,?4,?5,?6)",rusqlite::params![run_id,task_id,previous.map_or(1,|r|r.attempt+1),previous.map(|r|&r.id),session.session_id,turn_id]).map_err(|_|StoreError::Database)?;
    let sequence = event(
        tx,
        task_id,
        &run_id,
        if previous.is_some() {
            "run_retry_accepted"
        } else {
            "task_created"
        },
        "queued",
        Some(turn.event_sequence),
    )?;
    Ok(TaskReceipt {
        command_id: command.into(),
        task_id: task_id.into(),
        run_id,
        session_id: session.session_id,
        turn_id,
        event_sequence: sequence,
    })
}

fn insert_task(
    tx: &Transaction<'_>,
    spec: &TaskSpec,
    requested: &TaskSpec,
    resolved: Option<projects::ResolvedWorkspace>,
    batch: Option<(&str, u32)>,
    command: &str,
) -> StoreResult<TaskReceipt> {
    let id = opaque_id()?;
    tx.execute("INSERT INTO tasks(id,owner_node_id,spec,requested_spec,batch_id,combination_index,created_order) VALUES(?1,?2,?3,?4,?5,?6,(SELECT COALESCE(MAX(created_order),0)+1 FROM tasks))",
        rusqlite::params![id,query_node(tx)?.node_id,canonical(spec)?,canonical(requested)?,batch.map(|b|b.0),batch.map(|b|b.1)]).map_err(|_|StoreError::Database)?;
    insert_run(tx, &id, spec, resolved, None, command)
}

fn retry_allowed(
    connection: &Connection,
    previous: &RunResponse,
    acknowledge: bool,
) -> StoreResult<TaskResponse> {
    let current = task(connection, &previous.task_id)?;
    if current.latest_run.id != previous.id
        || !slop_core::orchestration::retryable(&previous.turn.status)
        || previous.attempt >= slop_core::orchestration::MAX_ATTEMPTS
    {
        return Err(StoreError::Conflict);
    }
    if previous.effects_unknown && !acknowledge {
        return Err(StoreError::Unsupported(
            "The previous attempt has unknown effects; inspect it and explicitly acknowledge them before retrying.",
        ));
    }
    Ok(current)
}

impl StoreClient {
    pub async fn task_instruction(
        &self,
        id: &str,
        request: SendMessageRequest,
    ) -> StoreResult<CommandReceipt> {
        if !self.shared.workspace_healthy.load(Ordering::Acquire) {
            return Err(StoreError::Unavailable);
        }
        let id = id.to_owned();
        let scope = format!("task:{id}:instruction");
        let payload = canonical(&request)?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) = projects::prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            if request.model.is_some()
                || request.settings.is_some()
                || request.delivery.unwrap_or_default() == DeliveryMode::AfterTurn
            {
                return Err(StoreError::Invalid);
            }
            let current = task(&tx, &id)?;
            let command = request.command_id.clone();
            let mut inner = request;
            inner.command_id = opaque_id()?;
            let mut receipt =
                send_message_tx(&tx, &current.latest_run.session_id, inner, None, None)?;
            receipt.command_id = command.clone();
            projects::save(&tx, &command, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }
    async fn prior_job<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        command: &str,
        scope: &str,
        payload: &str,
    ) -> StoreResult<Option<T>> {
        let (command, scope, payload) = (command.to_owned(), scope.to_owned(), payload.to_owned());
        self.submit(move |c| projects::prior(c, &command, &scope, &payload))
            .await
    }

    async fn resolve_job_project(
        &self,
        selection: &mut Option<slop_protocol::projects::ProjectWorkspaceRequest>,
    ) -> StoreResult<Option<projects::ResolvedWorkspace>> {
        if !self.shared.workspace_healthy.load(Ordering::Acquire) {
            return Err(StoreError::Unavailable);
        }
        let Some(selection) = selection else {
            return Ok(None);
        };
        let project = self.project(&selection.project_id).await?;
        let base_commit = self
            .shared
            .git
            .resolve(
                &projects::repository(&project),
                selection.base_ref.as_deref(),
            )
            .await?;
        selection.base_ref = Some(base_commit.clone());
        Ok(Some(projects::ResolvedWorkspace {
            project,
            base_commit,
            data_dir: self.shared.data_dir.clone(),
        }))
    }

    pub async fn create_task(
        &self,
        request: CreateTaskRequest,
        effective: Option<TaskSpec>,
    ) -> StoreResult<TaskReceipt> {
        let payload = canonical(&request)?;
        if let Some(receipt) = self
            .prior_job(&request.command_id, "create_task", &payload)
            .await?
        {
            return Ok(receipt);
        }
        let mut spec = effective.ok_or(StoreError::Invalid)?;
        let resolved = self.resolve_job_project(&mut spec.project).await?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) =
                projects::prior(&tx, &request.command_id, "create_task", &payload)?
            {
                return Ok(receipt);
            }
            let receipt = insert_task(
                &tx,
                &spec,
                &request.spec,
                resolved,
                None,
                &request.command_id,
            )?;
            projects::save(&tx, &request.command_id, "create_task", &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn task(&self, id: &str) -> StoreResult<TaskResponse> {
        let id = id.to_owned();
        self.submit(move |c| task(c, &id)).await
    }
    pub async fn run(&self, id: &str) -> StoreResult<RunResponse> {
        let id = id.to_owned();
        self.submit(move |c| run(c, &id)).await
    }

    pub async fn tasks(
        &self,
        batch: Option<&str>,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<TaskResponse>> {
        let batch = batch.map(str::to_owned);
        let limit = page_limit(limit)?;
        self.submit(move |c| {
            if let Some(id)=&batch { get_batch(c,id)?; }
            page(c,"SELECT id,created_order FROM tasks WHERE (?1 IS NULL OR batch_id=?1) AND created_order>?2 ORDER BY created_order LIMIT ?3",batch.as_deref(),after,limit,task)
        }).await
    }
    pub async fn runs(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<RunResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |c| {task(c,&id)?;page(c,"SELECT id,attempt FROM runs WHERE task_id=?1 AND attempt>?2 ORDER BY attempt LIMIT ?3",Some(&id),after,limit,run)}).await
    }
    pub async fn task_events(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<TaskEventResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |c| {
            let snapshot=task(c,&id)?;
            if after.unwrap_or(0)>snapshot.last_event_sequence {return Err(StoreError::Invalid);}
            let mut stmt=c.prepare("SELECT sequence,run_id,kind,status,session_event_sequence FROM task_events WHERE task_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(|_|StoreError::Database)?;
            let rows=stmt.query_map(rusqlite::params![id,after.unwrap_or(0).min(i64::MAX as u64) as i64,(limit+1) as i64],|r|Ok(TaskEventResponse{task_id:id.clone(),sequence:r.get::<_,i64>(0)? as u64,run_id:r.get(1)?,kind:r.get(2)?,status:r.get(3)?,session_event_sequence:r.get::<_,Option<i64>>(4)?.map(|s|s as u64)})).map_err(|_|StoreError::Database)?;
            let mut items=rows.collect::<Result<Vec<_>,_>>().map_err(|_|StoreError::Database)?;
            let more=items.len()>limit;items.truncate(limit);let next_after=more.then(||items.last().unwrap().sequence);Ok(Page{items,next_after})
        }).await
    }

    pub async fn retry_run(
        &self,
        id: &str,
        request: RetryRunRequest,
        effective: Option<TaskSpec>,
    ) -> StoreResult<TaskReceipt> {
        let scope = format!("run:{id}:retry");
        let payload = canonical(&request)?;
        if let Some(receipt) = self
            .prior_job(&request.command_id, &scope, &payload)
            .await?
        {
            return Ok(receipt);
        }
        let previous = self.run(id).await?;
        let mut spec = effective.ok_or(StoreError::Invalid)?;
        let resolved = self.resolve_job_project(&mut spec.project).await?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) = projects::prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let current = retry_allowed(
                &tx,
                &run(&tx, &previous.id)?,
                request.acknowledge_unknown_effects,
            )?;
            if current.spec != spec {
                return Err(StoreError::Conflict);
            }
            let receipt = insert_run(
                &tx,
                &previous.task_id,
                &spec,
                resolved,
                Some(&previous),
                &request.command_id,
            )?;
            projects::save(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn preview_batch(
        &self,
        mut spec: BatchSpec,
        mut cells: Vec<CombinationResponse>,
    ) -> StoreResult<BatchPreviewResponse> {
        // Validate cardinality before any repository I/O, contexts, or writes.
        combinations(&spec)?;
        if let Some(project) = &spec.project {
            self.require_workspace_capacity(&project.project_id, cells.len())
                .await?;
        }
        self.resolve_job_project(&mut spec.project).await?;
        for cell in &mut cells {
            cell.spec.project = spec.project.clone();
            validate_spec(&cell.spec)?;
        }
        let preview = BatchPreviewResponse {
            spec,
            combinations: cells,
        };
        if canonical(&preview)?.len() > 8 * 1024 * 1024 {
            return Err(StoreError::Limit);
        }
        Ok(preview)
    }

    pub async fn create_batch(
        &self,
        request: CreateBatchRequest,
        prepared: Option<BatchPreviewResponse>,
    ) -> StoreResult<BatchReceipt> {
        let payload = canonical(&request)?;
        if let Some(receipt) = self
            .prior_job(&request.command_id, "create_batch", &payload)
            .await?
        {
            return Ok(receipt);
        }
        let mut prepared = prepared.ok_or(StoreError::Invalid)?;
        let resolved = self.resolve_job_project(&mut prepared.spec.project).await?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) =
                projects::prior(&tx, &request.command_id, "create_batch", &payload)?
            {
                return Ok(receipt);
            }
            let id = opaque_id()?;
            tx.execute(
                "INSERT INTO batches(id,owner_node_id,spec,max_concurrent_runs,created_order)
                 VALUES(?1,?2,?3,?4,(SELECT COALESCE(MAX(created_order),0)+1 FROM batches))",
                rusqlite::params![
                    id,
                    query_node(&tx)?.node_id,
                    canonical(&prepared.spec)?,
                    prepared.spec.max_concurrent_runs
                ],
            )
            .map_err(|_| StoreError::Database)?;
            let requested = combinations(&request.spec)?;
            let mut members = Vec::with_capacity(prepared.combinations.len());
            for mut cell in prepared.combinations {
                cell.spec.project = prepared.spec.project.clone();
                let original = &requested
                    .get(cell.index as usize)
                    .ok_or(StoreError::Invalid)?
                    .spec;
                members.push(insert_task(
                    &tx,
                    &cell.spec,
                    original,
                    resolved.clone(),
                    Some((&id, cell.index)),
                    &request.command_id,
                )?);
            }
            let receipt = BatchReceipt {
                command_id: request.command_id.clone(),
                batch_id: id,
                members,
            };
            projects::save(&tx, &request.command_id, "create_batch", &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn batch(&self, id: &str) -> StoreResult<BatchResponse> {
        let id = id.to_owned();
        self.submit(move |c| get_batch(c, &id)).await
    }
    pub async fn batches(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<BatchResponse>> {
        let limit = page_limit(limit)?;
        self.submit(move |c|page(c,"SELECT id,created_order FROM batches WHERE (?1 IS NULL) AND created_order>?2 ORDER BY created_order LIMIT ?3",None,after,limit,get_batch)).await
    }
    pub async fn batch_results(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<BatchResultResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |c| {get_batch(c,&id)?;page(c,"SELECT id,created_order FROM tasks WHERE batch_id=?1 AND created_order>?2 ORDER BY created_order LIMIT ?3",Some(&id),after,limit,|c,id|{
            let task=task(c,id)?;
            let output=task.latest_run.turn.assistant_message_id.as_deref().map(|id|message(c,id)).transpose()?;
            Ok(BatchResultResponse{task,output})
        })}).await
    }

    pub async fn retry_batch(
        &self,
        id: &str,
        request: RetryBatchRequest,
        effective: Option<Vec<TaskSpec>>,
    ) -> StoreResult<BatchReceipt> {
        let scope = format!("batch:{id}:retry");
        let payload = canonical(&request)?;
        if let Some(receipt) = self
            .prior_job(&request.command_id, &scope, &payload)
            .await?
        {
            return Ok(receipt);
        }
        let specs = effective.ok_or(StoreError::Invalid)?;
        if request.indices.is_empty()
            || request.indices.len() > slop_core::orchestration::MAX_MATRIX_MEMBERS
            || specs.len() != request.indices.len()
        {
            return Err(StoreError::Invalid);
        }
        let mut unique = std::collections::BTreeSet::new();
        if request.indices.iter().any(|i| !unique.insert(i)) {
            return Err(StoreError::Invalid);
        }
        let mut project = specs[0].project.clone();
        let resolved = self.resolve_job_project(&mut project).await?;
        let id = id.to_owned();
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) = projects::prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            get_batch(&tx, &id)?;
            let mut members = Vec::new();
            for (index, spec) in request.indices.iter().zip(specs) {
                let task_id: String = tx
                    .query_row(
                        "SELECT id FROM tasks WHERE batch_id=?1 AND combination_index=?2",
                        rusqlite::params![id, index],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(|_| StoreError::Database)?
                    .ok_or(StoreError::NotFound)?;
                let current = task(&tx, &task_id)?;
                retry_allowed(
                    &tx,
                    &current.latest_run,
                    request.acknowledge_unknown_effects,
                )?;
                if current.spec != spec || spec.project != project {
                    return Err(StoreError::Conflict);
                }
                members.push(insert_run(
                    &tx,
                    &task_id,
                    &spec,
                    resolved.clone(),
                    Some(&current.latest_run),
                    &request.command_id,
                )?);
            }
            let receipt = BatchReceipt {
                command_id: request.command_id.clone(),
                batch_id: id,
                members,
            };
            projects::save(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn batch_task(&self, id: &str, index: u32) -> StoreResult<TaskResponse> {
        let id = id.to_owned();
        self.submit(move |c| {
            let task_id: String = c
                .query_row(
                    "SELECT id FROM tasks WHERE batch_id=?1 AND combination_index=?2",
                    rusqlite::params![id, index],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| StoreError::Database)?
                .ok_or(StoreError::NotFound)?;
            task(c, &task_id)
        })
        .await
    }
}

fn get_batch(c: &Connection, id: &str) -> StoreResult<BatchResponse> {
    let (owner, spec): (String, String) = c
        .query_row(
            "SELECT owner_node_id,spec FROM batches WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    let mut stmt=c.prepare("SELECT t.status,count(*) FROM tasks k JOIN runs r ON r.task_id=k.id JOIN turns t ON t.id=r.turn_id WHERE k.batch_id=?1 AND r.attempt=(SELECT MAX(attempt) FROM runs WHERE task_id=k.id) GROUP BY t.status").map_err(|_|StoreError::Database)?;
    let statuses = stmt
        .query_map([id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
        .map_err(|_| StoreError::Database)?;
    Ok(BatchResponse {
        id: id.into(),
        owner_node_id: owner,
        spec: serde_json::from_str(&spec).map_err(|_| StoreError::Database)?,
        total: statuses.values().sum(),
        statuses,
    })
}

fn page<T: serde::Serialize>(
    c: &Connection,
    sql: &str,
    scope: Option<&str>,
    after: Option<u64>,
    limit: usize,
    load: impl Fn(&Connection, &str) -> StoreResult<T>,
) -> StoreResult<Page<T>> {
    let mut stmt = c.prepare(sql).map_err(|_| StoreError::Database)?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                scope,
                after.unwrap_or(0).min(i64::MAX as u64) as i64,
                (limit + 1) as i64
            ],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
        )
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut cursor = after.unwrap_or(0);
    let mut bytes = 0;
    for row in rows {
        let (id, order) = row.map_err(|_| StoreError::Database)?;
        let value = load(c, &id)?;
        let size = canonical(&value)?.len();
        if items.len() == limit || !items.is_empty() && bytes + size > 4 * 1024 * 1024 {
            return Ok(Page {
                items,
                next_after: Some(cursor),
            });
        }
        bytes += size;
        cursor = order;
        items.push(value);
    }
    Ok(Page {
        items,
        next_after: None,
    })
}

fn message(c: &Connection, id: &str) -> StoreResult<MessageResponse> {
    let mut statement=c.prepare("SELECT id,session_id,turn_id,role,text,status,blocks,request_id FROM messages WHERE id=?1").map_err(|_|StoreError::Database)?;
    let mut rows = statement.query([id]).map_err(|_| StoreError::Database)?;
    let row = rows
        .next()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    Ok(MessageResponse {
        id: row.get(0).map_err(|_| StoreError::Database)?,
        session_id: row.get(1).map_err(|_| StoreError::Database)?,
        turn_id: row.get(2).map_err(|_| StoreError::Database)?,
        role: row.get(3).map_err(|_| StoreError::Database)?,
        text: row.get(4).map_err(|_| StoreError::Database)?,
        status: row.get(5).map_err(|_| StoreError::Database)?,
        blocks: execution::public_blocks_from_row(row, 6, 0, 4)?,
        request_id: row.get(7).map_err(|_| StoreError::Database)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_runtime::chat::{ChatOutcome, ChatStatus};

    async fn open(path: PathBuf) -> Store {
        Store::open(path, None, 64, Duration::from_secs(1))
            .await
            .unwrap()
    }
    fn spec(prompt: &str) -> TaskSpec {
        TaskSpec {
            title: Some("test job".into()),
            prompt: prompt.into(),
            model: "opencode-go/glm-5.3-flash".into(),
            settings: slop_protocol::execution::GenerationSettings {
                max_output_tokens: Some(17),
                reasoning_effort: None,
            },
            project: None,
        }
    }
    fn outcome(status: ChatStatus) -> ChatOutcome {
        ChatOutcome {
            text: "durable result".into(),
            resolved_model: Some("glm-5.3-flash".into()),
            usage: slop_runtime::providers::Usage::default(),
            status,
            error_code: None,
            error_message: None,
        }
    }
    async fn create(client: &StoreClient, command: &str, prompt: &str) -> TaskReceipt {
        let request = CreateTaskRequest {
            command_id: command.into(),
            spec: spec(prompt),
        };
        client
            .create_task(request.clone(), Some(request.spec))
            .await
            .unwrap()
    }
    fn batch_spec() -> BatchSpec {
        BatchSpec {
            name: "eight cells".into(),
            prompts: vec!["p0".into(), "p1".into()],
            models: vec![
                "opencode-go/glm-5.3-flash".into(),
                "opencode-go/glm-5.3".into(),
            ],
            settings: vec![
                slop_protocol::execution::GenerationSettings {
                    max_output_tokens: Some(17),
                    reasoning_effort: None,
                },
                slop_protocol::execution::GenerationSettings {
                    max_output_tokens: Some(18),
                    reasoning_effort: None,
                },
            ],
            project: None,
            max_concurrent_runs: 1,
            default_max_output_tokens: Some(4096),
        }
    }

    #[tokio::test]
    async fn requested_settings_remain_distinct_from_frozen_effective_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path().join("data")).await;
        let client = store.client();
        let effective = spec("defaults");
        let mut requested = effective.clone();
        requested.settings.max_output_tokens = None;
        let receipt = client
            .create_task(
                CreateTaskRequest {
                    command_id: "defaults-job".into(),
                    spec: requested.clone(),
                },
                Some(effective.clone()),
            )
            .await
            .unwrap();
        let task = client.task(&receipt.task_id).await.unwrap();
        assert_eq!(task.requested_spec, requested);
        assert_eq!(task.spec, effective);
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.requested_settings.max_output_tokens, None);
        assert_eq!(work.settings.max_output_tokens, Some(17));
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn job_acceptance_steering_pause_and_resume_share_durable_turn_facts() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path().join("data")).await;
        let client = store.client();
        let receipt = create(&client, "job", "initial prompt").await;
        let request = CreateTaskRequest {
            command_id: "job".into(),
            spec: spec("initial prompt"),
        };
        assert_eq!(
            client.create_task(request.clone(), None).await.unwrap(),
            receipt
        );
        let mut conflict = request;
        conflict.spec.prompt = "different".into();
        assert!(matches!(
            client.create_task(conflict, None).await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            client.claim_next_turn().await.unwrap().unwrap().turn_id,
            receipt.turn_id
        );
        let instruction = SendMessageRequest {
            command_id: "instruction".into(),
            text: "keep the interface".into(),
            delivery: Some(DeliveryMode::NextBoundary),
            ..Default::default()
        };
        let accepted = client
            .task_instruction(&receipt.task_id, instruction.clone())
            .await
            .unwrap();
        client
            .pause_turn(
                &receipt.turn_id,
                TurnControlRequest {
                    command_id: "pause".into(),
                },
            )
            .await
            .unwrap();
        client
            .finish_chat_turn(&receipt.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        assert_eq!(
            client.run(&receipt.run_id).await.unwrap().turn.status,
            "paused"
        );
        assert_eq!(
            client
                .task_instruction(&receipt.task_id, instruction.clone())
                .await
                .unwrap(),
            accepted
        );
        let resume = TurnControlRequest {
            command_id: "resume".into(),
        };
        let resumed = client
            .resume_turn(&receipt.turn_id, resume.clone())
            .await
            .unwrap();
        assert_eq!(
            client.resume_turn(&receipt.turn_id, resume).await.unwrap(),
            resumed
        );
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert!(work.messages.iter().any(|m| m.text == instruction.text));
        client
            .finish_chat_turn(&work.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        let current = client.task(&receipt.task_id).await.unwrap();
        assert_eq!(current.latest_run.turn.status, "completed");
        assert_eq!(current.latest_run.turn.settings.max_output_tokens, Some(17));
        let retry = RetryRunRequest {
            command_id: "retry-success".into(),
            acknowledge_unknown_effects: false,
        };
        assert!(matches!(
            client
                .retry_run(&receipt.run_id, retry, Some(current.spec))
                .await,
            Err(StoreError::Conflict)
        ));
        // Attempts cannot acquire extra primary turns through the chat API.
        assert!(matches!(
            client
                .send_message(
                    &receipt.session_id,
                    SendMessageRequest {
                        command_id: "extra-turn".into(),
                        text: "another job".into(),
                        ..Default::default()
                    }
                )
                .await,
            Err(StoreError::Conflict)
        ));
        let events = client
            .task_events(&receipt.task_id, None, 200)
            .await
            .unwrap();
        for kind in [
            "task_created",
            "turn_started",
            "steering_instruction_accepted",
            "turn_paused",
            "turn_resumed",
            "turn_completed",
        ] {
            assert!(
                events.items.iter().any(|e| e.kind == kind),
                "missing {kind}"
            );
        }
        assert_eq!(
            events.items.last().unwrap().sequence,
            current.last_event_sequence
        );
        let first = client.task_events(&receipt.task_id, None, 1).await.unwrap();
        assert_eq!(first.next_after, Some(1));
        let rest = client
            .task_events(&receipt.task_id, first.next_after, 200)
            .await
            .unwrap();
        assert_eq!(rest.items.len() + 1, events.items.len());
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn matrix_admission_releases_paused_slots_and_selective_retry_is_atomic() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path().join("data")).await;
        let client = store.client();
        let spec = batch_spec();
        let prepared = client
            .preview_batch(spec.clone(), combinations(&spec).unwrap())
            .await
            .unwrap();
        assert!(client.tasks(None, None, 50).await.unwrap().items.is_empty());
        let request = CreateBatchRequest {
            command_id: "matrix".into(),
            spec,
        };
        let batch = client
            .create_batch(request.clone(), Some(prepared))
            .await
            .unwrap();
        assert_eq!(batch.members.len(), 8);
        assert_eq!(client.create_batch(request, None).await.unwrap(), batch);
        let first = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(first.turn_id, batch.members[0].turn_id);
        assert!(client.claim_next_turn().await.unwrap().is_none());
        let independent = create(&client, "independent", "unrelated").await;
        assert_eq!(
            client.claim_next_turn().await.unwrap().unwrap().turn_id,
            independent.turn_id
        );
        client
            .finish_chat_turn(&independent.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        client
            .pause_turn(
                &first.turn_id,
                TurnControlRequest {
                    command_id: "pause-matrix".into(),
                },
            )
            .await
            .unwrap();
        client
            .finish_chat_turn(&first.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        let second = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(second.turn_id, batch.members[1].turn_id);
        assert!(client.claim_next_turn().await.unwrap().is_none());
        client
            .finish_chat_turn(&second.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        client
            .cancel_turn(
                &first.turn_id,
                CancelTurnRequest {
                    command_id: "cancel-first".into(),
                },
            )
            .await
            .unwrap();
        let before = client.task(&batch.members[0].task_id).await.unwrap();
        let success = client.task(&batch.members[1].task_id).await.unwrap();
        let invalid = RetryBatchRequest {
            command_id: "mixed-retry".into(),
            indices: vec![0, 1],
            acknowledge_unknown_effects: false,
        };
        assert!(matches!(
            client
                .retry_batch(
                    &batch.batch_id,
                    invalid,
                    Some(vec![before.spec.clone(), success.spec.clone()])
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert_eq!(
            client.runs(&before.id, None, 50).await.unwrap().items.len(),
            1
        );
        let request = RetryBatchRequest {
            command_id: "retry-first".into(),
            indices: vec![0],
            acknowledge_unknown_effects: false,
        };
        let retried = client
            .retry_batch(
                &batch.batch_id,
                request.clone(),
                Some(vec![before.spec.clone()]),
            )
            .await
            .unwrap();
        assert_eq!(
            client
                .retry_batch(&batch.batch_id, request, None)
                .await
                .unwrap(),
            retried
        );
        let current = client.task(&before.id).await.unwrap();
        assert_eq!(current.latest_run.attempt, 2);
        assert_eq!(
            current.latest_run.retry_of,
            Some(before.latest_run.id.clone())
        );
        assert_ne!(current.latest_run.session_id, before.latest_run.session_id);
        assert_eq!(client.task(&success.id).await.unwrap(), success);
        assert_eq!(client.batch(&batch.batch_id).await.unwrap().total, 8);
        let results = client
            .batch_results(&batch.batch_id, None, 2)
            .await
            .unwrap();
        assert_eq!(results.items.len(), 2);
        assert!(results.next_after.is_some());
        assert_eq!(
            results.items[1].output.as_ref().unwrap().text,
            "durable result"
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn recovery_never_replays_unknown_tools_and_retry_retains_the_old_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let store = open(data.clone()).await;
        let client = store.client();
        let receipt = create(&client, "interrupted", "work").await;
        client.claim_next_turn().await.unwrap().unwrap();
        client
            .checkpoint_visible(&receipt.turn_id, "partial")
            .await
            .unwrap();
        store.shutdown().await.unwrap();
        let store = open(data.clone()).await;
        let client = store.client();
        let current = client.task(&receipt.task_id).await.unwrap();
        assert_eq!(current.latest_run.turn.status, "interrupted");
        assert_eq!(
            current.latest_run.turn.error_code.as_deref(),
            Some("daemon_restarted")
        );
        assert!(client.claim_next_turn().await.unwrap().is_none());
        let turn = receipt.turn_id.clone();
        // A completed recovery fact with unknown external effects is retained.
        client.submit(move |c| {
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            let user:String=tx.query_row("SELECT user_message_id FROM turns WHERE id=?1",[&turn],|r|r.get(0)).map_err(|_|StoreError::Database)?;
            tx.execute("INSERT INTO model_requests(id,turn_id,message_id,ordinal,requested_model,requested_settings,settings,status) VALUES('uncertain-request',?1,?2,1,'opencode-go/glm-5.3-flash','{}','{}','failed')",rusqlite::params![turn,user]).map_err(|_|StoreError::Database)?;
            tx.execute("INSERT INTO tool_invocations(id,turn_id,request_id,call_id,provider_call_id,result_message_id,name,arguments,status,outcome) VALUES('uncertain-tool',?1,'uncertain-request','uncertain-call','provider-call','result-id','bash','{}','failed',?2)",rusqlite::params![turn,canonical(&slop_runtime::tools::ToolOutcome::failed("daemon_restarted",true))?]).map_err(|_|StoreError::Database)?;
            tx.commit().map_err(|_|StoreError::Database)
        }).await.unwrap();
        let request = RetryRunRequest {
            command_id: "explicit-retry".into(),
            acknowledge_unknown_effects: false,
        };
        assert!(matches!(
            client
                .retry_run(&receipt.run_id, request.clone(), Some(current.spec.clone()))
                .await,
            Err(StoreError::Unsupported(_))
        ));
        let mut request = request;
        request.acknowledge_unknown_effects = true;
        let retried = client
            .retry_run(&receipt.run_id, request.clone(), Some(current.spec))
            .await
            .unwrap();
        assert_eq!(
            client
                .retry_run(&receipt.run_id, request.clone(), None)
                .await
                .unwrap(),
            retried
        );
        assert_eq!(
            client.run(&receipt.run_id).await.unwrap().turn.status,
            "interrupted"
        );
        assert!(client.run(&receipt.run_id).await.unwrap().effects_unknown);
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, retried.turn_id);
        assert_eq!(work.messages.len(), 1);
        assert_eq!(work.messages[0].text, "work");
        assert!(
            work.history
                .iter()
                .all(|m| m.role != slop_runtime::providers::inference::MessageRole::Assistant)
        );
        client
            .finish_chat_turn(&work.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        let events = client
            .task_events(&receipt.task_id, None, 200)
            .await
            .unwrap();
        assert_eq!(
            events
                .items
                .iter()
                .filter(|e| e.kind == "turn_interrupted")
                .count(),
            1
        );
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let client = store.client();
        assert_eq!(
            client
                .retry_run(&receipt.run_id, request, None)
                .await
                .unwrap(),
            retried
        );
        assert!(client.claim_next_turn().await.unwrap().is_none());
        assert_eq!(
            client
                .task_events(&receipt.task_id, None, 200)
                .await
                .unwrap(),
            events
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn acceptance_event_failure_rolls_back_all_batch_entities_and_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let store = open(directory.path().join("data")).await;
        let client = store.client();
        client.submit(|c|c.execute_batch("CREATE TRIGGER reject_job_event BEFORE INSERT ON task_events BEGIN SELECT RAISE(ABORT,'injected'); END;").map_err(|_|StoreError::Database)).await.unwrap();
        let spec = batch_spec();
        let prepared = BatchPreviewResponse {
            combinations: combinations(&spec).unwrap(),
            spec: spec.clone(),
        };
        let request = CreateBatchRequest {
            command_id: "atomic-batch".into(),
            spec,
        };
        assert!(matches!(
            client
                .create_batch(request.clone(), Some(prepared.clone()))
                .await,
            Err(StoreError::Database)
        ));
        client
            .submit(|c| {
                for table in [
                    "tasks",
                    "runs",
                    "batches",
                    "sessions",
                    "turns",
                    "messages",
                    "commands",
                    "session_events",
                ] {
                    let count: i64 = c
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                        .map_err(|_| StoreError::Database)?;
                    assert_eq!(count, 0, "{table}");
                }
                c.execute_batch("DROP TRIGGER reject_job_event")
                    .map_err(|_| StoreError::Database)
            })
            .await
            .unwrap();
        assert_eq!(
            client
                .create_batch(request, Some(prepared))
                .await
                .unwrap()
                .members
                .len(),
            8
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn schema_five_migration_preserves_legacy_session_receipts_and_identity() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let store = open(data.clone()).await;
        let client = store.client();
        let node = client.node().await.unwrap();
        let request = CreateSessionRequest {
            command_id: "legacy-five".into(),
            provider: "opencode-go".into(),
            model: "glm-5.3-flash".into(),
            ..Default::default()
        };
        let receipt = client.create_session(request.clone()).await.unwrap();
        let snapshot = client.session(&receipt.session_id).await.unwrap();
        client.submit(|c|c.execute_batch("DROP TABLE task_events; DROP TABLE runs; DROP TABLE tasks; DROP TABLE batches; PRAGMA user_version=5;").map_err(|_|StoreError::Database)).await.unwrap();
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let client = store.client();
        assert_eq!(client.node().await.unwrap(), node);
        assert_eq!(client.session(&receipt.session_id).await.unwrap(), snapshot);
        assert_eq!(client.create_session(request).await.unwrap(), receipt);
        assert!(client.tasks(None, None, 50).await.unwrap().items.is_empty());
        create(&client, "new-six", "job after upgrade").await;
        store.shutdown().await.unwrap();
    }
}
