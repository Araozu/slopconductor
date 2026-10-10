//! Durable repository registration and lazy, one-workspace-per-session leases.
use super::*;
use slop_protocol::projects::*;
use slop_runtime::{
    git::{GitError, Repository},
    tools::WorkspacePolicy,
};

const MAX_PROJECTS: i64 = 256;
const MAX_WORKSPACES: i64 = 256;
const MAX_PROJECT_WORKSPACES: i64 = 32;

#[derive(Clone)]
pub(super) struct ResolvedWorkspace {
    pub(super) project: ProjectResponse,
    pub(super) base_commit: String,
    pub(super) data_dir: PathBuf,
}

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("CREATE TABLE projects (
        created_order INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,
        owner_node_id TEXT NOT NULL,path TEXT NOT NULL,git_common_dir TEXT NOT NULL UNIQUE);
        CREATE TABLE managed_workspaces (
        created_order INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,
        project_id TEXT NOT NULL REFERENCES projects(id),session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id),
        owner_node_id TEXT NOT NULL,path TEXT NOT NULL UNIQUE,base_commit TEXT NOT NULL,
        status TEXT NOT NULL,error_code TEXT,effects_unknown INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX workspaces_project ON managed_workspaces(project_id,created_order);
        CREATE TABLE project_events (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,project_id TEXT NOT NULL REFERENCES projects(id),
        workspace_id TEXT REFERENCES managed_workspaces(id),kind TEXT NOT NULL);
        CREATE INDEX project_events_project ON project_events(project_id,sequence);
        ALTER TABLE sessions ADD COLUMN managed_workspace_id TEXT REFERENCES managed_workspaces(id);")
        .map_err(|_| StoreError::Database)
}

fn project_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectResponse> {
    Ok(ProjectResponse {
        id: row.get(0)?,
        owner_node_id: row.get(1)?,
        path: row.get(2)?,
        git_common_dir: row.get(3)?,
    })
}
fn workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceResponse> {
    Ok(WorkspaceResponse {
        id: row.get(0)?,
        project_id: row.get(1)?,
        session_id: row.get(2)?,
        owner_node_id: row.get(3)?,
        path: row.get(4)?,
        base_commit: row.get(5)?,
        status: row.get(6)?,
        error_code: row.get(7)?,
        effects_unknown: row.get(8)?,
    })
}
fn project(connection: &Connection, id: &str) -> StoreResult<ProjectResponse> {
    connection
        .query_row(
            "SELECT id,owner_node_id,path,git_common_dir FROM projects WHERE id=?1",
            [id],
            project_row,
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)
}
fn workspace(connection: &Connection, id: &str) -> StoreResult<WorkspaceResponse> {
    connection.query_row("SELECT id,project_id,session_id,owner_node_id,path,base_commit,status,error_code,effects_unknown FROM managed_workspaces WHERE id=?1", [id], workspace_row)
        .optional().map_err(|_| StoreError::Database)?.ok_or(StoreError::NotFound)
}
pub(super) fn repository(project: &ProjectResponse) -> Repository {
    Repository {
        path: project.path.clone(),
        common_dir: project.git_common_dir.clone(),
    }
}
fn event(
    tx: &Transaction<'_>,
    project: &str,
    workspace: Option<&str>,
    kind: &str,
) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO project_events(project_id,workspace_id,kind) VALUES(?1,?2,?3)",
        rusqlite::params![project, workspace, kind],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

pub(super) fn prior<T: serde::de::DeserializeOwned>(
    connection: &Connection,
    command_id: &str,
    scope: &str,
    payload: &str,
) -> StoreResult<Option<T>> {
    if !valid_command_id(command_id) {
        return Err(StoreError::Invalid);
    }
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
pub(super) fn save<T: serde::Serialize>(
    tx: &Transaction<'_>,
    command: &str,
    scope: &str,
    payload: &str,
    receipt: &T,
) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO commands(command_id,scope,payload,receipt) VALUES(?1,?2,?3,?4)",
        rusqlite::params![command, scope, payload, canonical(receipt)?],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

pub(super) fn reserve(
    tx: &Transaction<'_>,
    session: &str,
    request: &ProjectWorkspaceRequest,
    resolved: &ResolvedWorkspace,
) -> StoreResult<(WorkspaceResponse, WorkspacePolicy)> {
    let registered = project(tx, &request.project_id)?;
    if registered != resolved.project {
        return Err(StoreError::Conflict);
    }
    let (global, local): (i64, i64) = tx.query_row("SELECT COUNT(*),COUNT(CASE WHEN project_id=?1 THEN 1 END) FROM managed_workspaces WHERE status!='removed'", [&request.project_id], |row| Ok((row.get(0)?, row.get(1)?))).map_err(|_| StoreError::Database)?;
    if global >= MAX_WORKSPACES || local >= MAX_PROJECT_WORKSPACES {
        return Err(StoreError::Limit);
    }
    let id = opaque_id()?;
    let path = resolved
        .data_dir
        .join("workspaces")
        .join(&registered.id)
        .join(&id)
        .to_str()
        .ok_or(StoreError::Invalid)?
        .to_owned();
    let policy = WorkspacePolicy {
        root: path.clone(),
        allowed_tools: request.allowed_tools.clone(),
        shell_timeout_ms: 30_000,
        max_output_bytes: 1024 * 1024,
        max_tool_calls: 32,
        max_model_requests: 16,
    };
    policy.validate().map_err(|_| StoreError::Invalid)?;
    if request.project_id.len() > 128 {
        return Err(StoreError::Invalid);
    }
    Ok((
        WorkspaceResponse {
            id,
            project_id: registered.id,
            session_id: session.to_owned(),
            owner_node_id: registered.owner_node_id,
            path,
            base_commit: resolved.base_commit.clone(),
            status: "reserved".into(),
            error_code: None,
            effects_unknown: false,
        },
        policy,
    ))
}

pub(super) fn insert_workspace(
    tx: &Transaction<'_>,
    workspace: &WorkspaceResponse,
) -> StoreResult<(u64, u64)> {
    tx.execute("INSERT INTO managed_workspaces(id,project_id,session_id,owner_node_id,path,base_commit,status) VALUES(?1,?2,?3,?4,?5,?6,'reserved')", rusqlite::params![workspace.id, workspace.project_id, workspace.session_id, workspace.owner_node_id, workspace.path, workspace.base_commit]).map_err(|_| StoreError::Database)?;
    tx.execute(
        "UPDATE sessions SET managed_workspace_id=?1 WHERE id=?2",
        rusqlite::params![workspace.id, workspace.session_id],
    )
    .map_err(|_| StoreError::Database)?;
    event(
        tx,
        &workspace.project_id,
        Some(&workspace.id),
        "workspace_reserved",
    )?;
    append_event(tx, &workspace.session_id, "workspace_reserved", None, None)
}

fn transition(
    tx: &Transaction<'_>,
    workspace: &WorkspaceResponse,
    state: &str,
    kind: &str,
    error: Option<&str>,
    effects_unknown: bool,
) -> StoreResult<()> {
    tx.execute(
        "UPDATE managed_workspaces SET status=?1,error_code=?2,effects_unknown=?3 WHERE id=?4",
        rusqlite::params![state, error, effects_unknown, workspace.id],
    )
    .map_err(|_| StoreError::Database)?;
    event(tx, &workspace.project_id, Some(&workspace.id), kind)?;
    append_event(tx, &workspace.session_id, kind, None, None)?;
    Ok(())
}

pub(super) fn require_available(
    connection: &Connection,
    session: &SessionResponse,
) -> StoreResult<()> {
    if let Some(root) = session.execution.as_ref().map(|policy| &policy.root) {
        let status: Option<String> = connection
            .query_row(
                "SELECT status FROM managed_workspaces WHERE path=?1",
                [root],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StoreError::Database)?;
        if status
            .is_some_and(|status| !matches!(status.as_str(), "reserved" | "allocating" | "ready"))
        {
            return Err(StoreError::Preflight(
                "workspace_unavailable",
                "The managed workspace requires inspection or has been removed.",
            ));
        }
    }
    Ok(())
}

pub(super) fn recover(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut statement = tx.prepare("SELECT id,project_id,session_id,owner_node_id,path,base_commit,status,error_code,effects_unknown FROM managed_workspaces WHERE status IN('allocating','removing')").map_err(|_| StoreError::Database)?;
    let workspaces = statement
        .query_map([], workspace_row)
        .map_err(|_| StoreError::Database)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StoreError::Database)?;
    for workspace in workspaces {
        transition(
            tx,
            &workspace,
            "failed",
            "workspace_failed",
            Some("daemon_restarted"),
            true,
        )?;
    }
    Ok(())
}

impl StoreClient {
    pub(super) async fn require_workspace_capacity(
        &self,
        id: &str,
        needed: usize,
    ) -> StoreResult<()> {
        let id = id.to_owned();
        self.submit(move |c| {
            project(c,&id)?;
            let (global,local):(i64,i64)=c.query_row("SELECT count(*),count(CASE WHEN project_id=?1 THEN 1 END) FROM managed_workspaces WHERE status!='removed'",[id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|StoreError::Database)?;
            if global+needed as i64>MAX_WORKSPACES || local+needed as i64>MAX_PROJECT_WORKSPACES {Err(StoreError::Limit)} else {Ok(())}
        }).await
    }
    pub async fn register_project(
        &self,
        request: RegisterProjectRequest,
    ) -> StoreResult<ProjectResponse> {
        let payload = canonical(&request)?;
        let command = request.command_id.clone();
        let query_payload = payload.clone();
        if let Some(prior) = self
            .submit(move |connection| {
                prior(connection, &command, "register_project", &query_payload)
            })
            .await?
        {
            return Ok(prior);
        }
        let repo = self.shared.git.inspect(&request.path).await?;
        self.submit(move |connection| {
            let tx = connection.unchecked_transaction().map_err(|_| StoreError::Database)?;
            if let Some(prior) = prior(&tx, &request.command_id, "register_project", &payload)? { return Ok(prior); }
            let existing = tx.query_row("SELECT id,owner_node_id,path,git_common_dir FROM projects WHERE git_common_dir=?1", [&repo.common_dir], project_row).optional().map_err(|_| StoreError::Database)?;
            let registered = match existing {
                Some(project) => project,
                None => {
                    let count: i64 = tx.query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0)).map_err(|_| StoreError::Database)?;
                    if count >= MAX_PROJECTS { return Err(StoreError::Limit); }
                    let project = ProjectResponse { id: opaque_id()?, owner_node_id: query_node(&tx)?.node_id, path: repo.path, git_common_dir: repo.common_dir };
                    tx.execute("INSERT INTO projects(id,owner_node_id,path,git_common_dir) VALUES(?1,?2,?3,?4)", rusqlite::params![project.id, project.owner_node_id, project.path, project.git_common_dir]).map_err(|_| StoreError::Database)?;
                    event(&tx, &project.id, None, "project_registered")?;
                    project
                }
            };
            save(&tx, &request.command_id, "register_project", &payload, &registered)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(registered)
        }).await
    }

    pub async fn project(&self, id: &str) -> StoreResult<ProjectResponse> {
        let id = id.to_owned();
        self.submit(move |connection| project(connection, &id))
            .await
    }
    pub async fn workspace(&self, id: &str) -> StoreResult<WorkspaceResponse> {
        let id = id.to_owned();
        self.submit(move |connection| workspace(connection, &id))
            .await
    }
    pub async fn projects(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<ProjectResponse>> {
        let limit = page_limit(limit)?;
        self.submit(move |connection| {
            page(connection, "SELECT id,owner_node_id,path,git_common_dir,created_order FROM projects WHERE created_order>?1 ORDER BY created_order LIMIT ?2", None, after, limit, 4, project_row)
        }).await
    }
    pub async fn workspaces(
        &self,
        project_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<WorkspaceResponse>> {
        let id = project_id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |connection| {
            project(connection, &id)?;
            page(connection, "SELECT id,project_id,session_id,owner_node_id,path,base_commit,status,error_code,effects_unknown,created_order FROM managed_workspaces WHERE created_order>?1 AND project_id=?3 ORDER BY created_order LIMIT ?2", Some(&id), after, limit, 9, workspace_row)
        }).await
    }
    pub async fn project_events(
        &self,
        project_id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<ProjectEventResponse>> {
        let id = project_id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move |connection| {
            project(connection, &id)?;
            page(connection, "SELECT sequence,project_id,workspace_id,kind FROM project_events WHERE sequence>?1 AND project_id=?3 ORDER BY sequence LIMIT ?2", Some(&id), after, limit, 0, |row| Ok(ProjectEventResponse { sequence: row.get::<_, i64>(0)? as u64, project_id: row.get(1)?, workspace_id: row.get(2)?, kind: row.get(3)? }))
        }).await
    }

    pub(super) async fn resolve_project_workspace(
        &self,
        request: &CreateSessionRequest,
    ) -> StoreResult<Option<ResolvedWorkspace>> {
        let Some(selection) = &request.project else {
            return Ok(None);
        };
        if request.execution.is_some() {
            return Err(StoreError::Invalid);
        }
        let command = request.command_id.clone();
        let payload = canonical(request)?;
        if self
            .submit(move |connection| {
                prior_receipt(connection, &command, "create_session", &payload)
            })
            .await?
            .is_some()
        {
            return Ok(None);
        }
        let project = self.project(&selection.project_id).await?;
        let base_commit = self
            .shared
            .git
            .resolve(&repository(&project), selection.base_ref.as_deref())
            .await?;
        Ok(Some(ResolvedWorkspace {
            project,
            base_commit,
            data_dir: self.shared.data_dir.clone(),
        }))
    }

    /// Called only after a turn has an admission slot and a committed running claim.
    pub(super) async fn prepare_workspace(
        &self,
        turn: &str,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> StoreResult<Option<(&'static str, &'static str)>> {
        let turn_id = turn.to_owned();
        let work = self.submit(move |connection| {
            let tx = connection.unchecked_transaction().map_err(|_| StoreError::Database)?;
            let id: Option<String> = tx.query_row("SELECT w.id FROM turns t JOIN sessions s ON s.id=t.session_id JOIN managed_workspaces w ON w.path=s.workspace_root WHERE t.id=?1", [&turn_id], |row| row.get(0)).optional().map_err(|_| StoreError::Database)?;
            let Some(id) = id else { return Ok(None); };
            let workspace = workspace(&tx, &id)?;
            if workspace.status == "reserved" { transition(&tx, &workspace, "allocating", "workspace_allocating", None, false)?; }
            let project = project(&tx, &workspace.project_id)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(Some((workspace, project)))
        }).await?;
        let Some((workspace, project)) = work else {
            return Ok(None);
        };
        if !matches!(workspace.status.as_str(), "reserved" | "ready") {
            return Ok(Some((
                "workspace_unavailable",
                "The managed workspace requires inspection; Git operations are not replayed.",
            )));
        }
        let result = {
            let repo = repository(&project);
            let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
            let operation = async {
                if workspace.status == "reserved" {
                    self.shared
                        .git
                        .allocate(
                            &repo,
                            &workspace.path,
                            &workspace.base_commit,
                            &mut cancelled,
                        )
                        .await
                } else {
                    self.shared
                        .git
                        .validate_workspace(&repo, &workspace.path, &mut cancelled)
                        .await
                }
            };
            tokio::pin!(operation);
            loop {
                tokio::select! {
                    result = &mut operation => break result,
                    _ = shutdown.changed() => { let _ = cancel.send(true); break operation.await; },
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        let control = self.turn_control(turn).await?;
                        if control.cancellation_requested { let _ = cancel.send(true); break operation.await; }
                    },
                }
            }
        };
        if result.is_ok() && workspace.status == "ready" {
            return Ok(None);
        }
        let failure = result.as_ref().err().map(|error| {
            (
                error.code,
                "Managed worktree allocation failed; inspect its durable workspace record.",
            )
        });
        let outcome = result.err();
        self.persist_workspace_outcome(
            &workspace,
            if outcome.is_some() { "failed" } else { "ready" },
            if outcome.is_some() {
                "workspace_failed"
            } else {
                "workspace_ready"
            },
            outcome,
        )
        .await?;
        Ok(failure)
    }

    pub async fn workspace_diff(&self, id: &str) -> StoreResult<WorkspaceDiffResponse> {
        let workspace = self.workspace(id).await?;
        if !matches!(workspace.status.as_str(), "ready" | "failed") {
            return Err(StoreError::Conflict);
        }
        let path = workspace.path.clone();
        if self
            .submit(move |connection| busy(connection, &path, false))
            .await?
        {
            return Err(StoreError::Conflict);
        }
        let project = self.project(&workspace.project_id).await?;
        let diff = self
            .shared
            .git
            .diff(
                &repository(&project),
                &workspace.path,
                &workspace.base_commit,
            )
            .await?;
        Ok(WorkspaceDiffResponse {
            workspace_id: workspace.id,
            base_commit: workspace.base_commit,
            head_commit: diff.head_commit,
            patch: diff.patch,
            status: diff.status,
        })
    }

    /// Persist intent before spawning cleanup, so client lifetime cannot own it.
    pub async fn remove_workspace(
        &self,
        id: &str,
        request: RemoveWorkspaceRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let mut jobs = self.shared.workspace_jobs.lock().await;
        while jobs.try_join_next().is_some() {}
        let capacity = jobs.len() < 2;
        let id = id.to_owned();
        let payload = canonical(&request)?;
        let scope = format!("workspace:{id}:remove");
        let (response, launch) = self
            .submit(move |connection| {
                let tx = connection
                    .unchecked_transaction()
                    .map_err(|_| StoreError::Database)?;
                if let Some(prior) = prior(&tx, &request.command_id, &scope, &payload)? {
                    return Ok((prior, false));
                }
                let workspace = workspace(&tx, &id)?;
                if workspace.status == "ready" && !capacity {
                    return Err(StoreError::Busy);
                }
                if busy(&tx, &workspace.path, true)?
                    || !matches!(workspace.status.as_str(), "reserved" | "ready" | "removed")
                {
                    return Err(StoreError::Conflict);
                }
                let pending = workspace.status == "reserved";
                if workspace.status != "removed" {
                    transition(
                        &tx,
                        &workspace,
                        if pending { "removed" } else { "removing" },
                        if pending {
                            "workspace_removed"
                        } else {
                            "workspace_removing"
                        },
                        None,
                        false,
                    )?;
                }
                let response = self::workspace(&tx, &id)?;
                save(&tx, &request.command_id, &scope, &payload, &response)?;
                tx.commit().map_err(|_| StoreError::Database)?;
                Ok((response, workspace.status == "ready"))
            })
            .await?;
        if launch {
            let store = self.clone();
            let workspace = response.clone();
            jobs.spawn(async move {
                let outcome = async {
                    let project = store.project(&workspace.project_id).await?;
                    let (_cancel, mut cancelled) = tokio::sync::watch::channel(false);
                    store
                        .shared
                        .git
                        .remove(
                            &repository(&project),
                            &workspace.path,
                            &workspace.base_commit,
                            &mut cancelled,
                        )
                        .await
                        .map_err(StoreError::from)
                }
                .await;
                let error = match outcome {
                    Ok(()) => None,
                    Err(StoreError::Git(error)) => Some(error),
                    Err(_) => Some(GitError {
                        code: "storage_unavailable",
                        effects_unknown: false,
                    }),
                };
                let state = match &error {
                    None => "removed",
                    Some(error) if !error.effects_unknown => "ready",
                    Some(_) => "failed",
                };
                let _ = store
                    .persist_workspace_outcome(
                        &workspace,
                        state,
                        if error.is_none() {
                            "workspace_removed"
                        } else {
                            "workspace_cleanup_failed"
                        },
                        error,
                    )
                    .await;
            });
        }
        Ok(response)
    }

    pub async fn drain_workspace_jobs(&self) {
        let mut jobs = self.shared.workspace_jobs.lock().await;
        while jobs.join_next().await.is_some() {}
    }

    async fn persist_workspace_outcome(
        &self,
        record: &WorkspaceResponse,
        state: &'static str,
        kind: &'static str,
        error: Option<GitError>,
    ) -> StoreResult<()> {
        // Retry only this database write. Git has already finished and is never
        // repeated, including after an ambiguous persistence acknowledgement.
        for _ in 0..50 {
            let record = record.clone();
            let error = error.clone();
            if self
                .submit(move |connection| {
                    let tx = connection
                        .unchecked_transaction()
                        .map_err(|_| StoreError::Database)?;
                    let current = workspace(&tx, &record.id)?;
                    let code = error.as_ref().map(|error| error.code);
                    let unknown = error.as_ref().is_some_and(|error| error.effects_unknown);
                    if current.status == state
                        && current.error_code.as_deref() == code
                        && current.effects_unknown == unknown
                    {
                        return Ok(());
                    }
                    transition(&tx, &record, state, kind, code, unknown)?;
                    tx.commit().map_err(|_| StoreError::Database)
                })
                .await
                .is_ok()
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.shared
            .workspace_healthy
            .store(false, Ordering::Release);
        Err(StoreError::Unavailable)
    }
}

fn busy(connection: &Connection, path: &str, queued: bool) -> StoreResult<bool> {
    connection.query_row("SELECT EXISTS(SELECT 1 FROM turns t JOIN sessions s ON s.id=t.session_id WHERE s.workspace_root=?1 AND (t.status='running' OR (?2 AND t.status IN('queued','paused'))))", rusqlite::params![path, queued], |row| row.get(0)).map_err(|_| StoreError::Database)
}
fn page<T>(
    connection: &Connection,
    sql: &str,
    id: Option<&str>,
    after: Option<u64>,
    limit: usize,
    order_column: usize,
    mut map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> StoreResult<Page<T>> {
    let mut statement = connection.prepare(sql).map_err(|_| StoreError::Database)?;
    let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let mut rows = if let Some(id) = id {
        statement.query(rusqlite::params![after, (limit + 1) as i64, id])
    } else {
        statement.query(rusqlite::params![after, (limit + 1) as i64])
    }
    .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut cursor = 0;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            has_more = true;
            break;
        }
        cursor = row
            .get::<_, i64>(order_column)
            .map_err(|_| StoreError::Database)? as u64;
        items.push(map(row).map_err(|_| StoreError::Database)?);
    }
    Ok(Page {
        items,
        next_after: has_more.then_some(cursor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open(path: PathBuf) -> Store {
        Store::open(path, None, 64, Duration::from_secs(1))
            .await
            .unwrap()
    }

    // Inject durable boundaries without invoking Git or a model. Recovery must
    // treat a journaled operation with no outcome conservatively in either case.
    async fn seed(
        client: &StoreClient,
        data_dir: PathBuf,
        command: &str,
        state: &str,
    ) -> WorkspaceResponse {
        let command = command.to_owned();
        let state = state.to_owned();
        client.submit(move |connection| {
            connection.execute("INSERT OR IGNORE INTO projects(id,owner_node_id,path,git_common_dir) VALUES('project','owner','/unavailable/repo','/unavailable/repo/.git')", []).unwrap();
            let project = project(connection, "project")?;
            let request = CreateSessionRequest {
                command_id: command,
                provider: "opencode-go".into(), model: "glm-5.3-flash".into(),
                project: Some(ProjectWorkspaceRequest { project_id: "project".into(), base_ref: None, allowed_tools: vec!["edit".into()] }),
                ..Default::default()
            };
            let receipt = create_session(connection, request, Some(1024), Some(ResolvedWorkspace { project, base_commit: "a".repeat(40), data_dir }))?;
            let session = get_session(connection, &receipt.session_id)?;
            let workspace = workspace(connection, session.managed_workspace_id.as_deref().unwrap())?;
            let tx = connection.unchecked_transaction().unwrap();
            transition(&tx, &workspace, &state, "injected_boundary", None, false)?;
            tx.commit().unwrap();
            self::workspace(connection, &workspace.id)
        }).await.unwrap()
    }

    #[tokio::test]
    async fn restart_fails_unfinished_git_operations_without_touching_paths_or_replaying() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let store = open(data.clone()).await;
        let client = store.client();
        let data = fs::canonicalize(data).unwrap();
        let reserved = seed(&client, data.clone(), "reserved", "reserved").await;
        let allocating = seed(&client, data.clone(), "allocating", "allocating").await;
        let removing = seed(&client, data.clone(), "removing", "removing").await;
        let ready = seed(&client, data.clone(), "ready", "ready").await;
        fs::create_dir_all(&removing.path).unwrap();
        fs::write(Path::new(&removing.path).join("retain.txt"), "retain").unwrap();
        store.shutdown().await.unwrap();

        let store = open(data.clone()).await;
        let client = store.client();
        for original in [&allocating, &removing] {
            let recovered = client.workspace(&original.id).await.unwrap();
            assert_eq!(recovered.status, "failed");
            assert_eq!(recovered.error_code.as_deref(), Some("daemon_restarted"));
            assert!(recovered.effects_unknown);
            let error = client
                .send_message(
                    &original.session_id,
                    SendMessageRequest {
                        command_id: format!("send-{}", original.id),
                        text: "continue".into(),
                        ..Default::default()
                    },
                )
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                StoreError::Preflight("workspace_unavailable", _)
            ));
            assert!(matches!(
                client
                    .remove_workspace(
                        &original.id,
                        RemoveWorkspaceRequest {
                            command_id: format!("remove-{}", original.id)
                        }
                    )
                    .await,
                Err(StoreError::Conflict)
            ));
        }
        assert_eq!(client.workspace(&reserved.id).await.unwrap(), reserved);
        assert_eq!(client.workspace(&ready.id).await.unwrap(), ready);
        assert!(!Path::new(&reserved.path).exists());
        assert!(!Path::new(&allocating.path).exists());
        assert_eq!(
            fs::read_to_string(Path::new(&removing.path).join("retain.txt")).unwrap(),
            "retain"
        );
        let events = client
            .project_events("project", None, 200)
            .await
            .unwrap()
            .items;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "workspace_failed")
                .count(),
            2
        );
        store.shutdown().await.unwrap();
        let store = open(data).await;
        assert_eq!(
            store
                .client()
                .project_events("project", None, 200)
                .await
                .unwrap()
                .items,
            events
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn outcome_persistence_retries_atomically_without_duplicate_events() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let store = open(data.clone()).await;
        let client = store.client();
        let record = seed(
            &client,
            fs::canonicalize(data).unwrap(),
            "persist-outcome",
            "allocating",
        )
        .await;
        client.submit(|connection| {
            connection.execute_batch("CREATE TRIGGER fail_workspace_event BEFORE INSERT ON project_events WHEN NEW.kind='workspace_ready' BEGIN SELECT RAISE(ABORT,'injected persistence failure'); END;").map_err(|_| StoreError::Database)
        }).await.unwrap();
        let writer = client.clone();
        let saved = record.clone();
        let handle = tokio::spawn(async move {
            writer
                .persist_workspace_outcome(&saved, "ready", "workspace_ready", None)
                .await
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!handle.is_finished());
        assert_eq!(
            client.workspace(&record.id).await.unwrap().status,
            "allocating"
        );
        assert!(
            !client
                .project_events("project", None, 200)
                .await
                .unwrap()
                .items
                .iter()
                .any(|event| event.kind == "workspace_ready")
        );
        client
            .submit(|connection| {
                connection
                    .execute_batch("DROP TRIGGER fail_workspace_event")
                    .map_err(|_| StoreError::Database)
            })
            .await
            .unwrap();
        handle.await.unwrap().unwrap();
        client
            .persist_workspace_outcome(&record, "ready", "workspace_ready", None)
            .await
            .unwrap();
        assert_eq!(
            client
                .project_events("project", None, 200)
                .await
                .unwrap()
                .items
                .iter()
                .filter(|event| event.kind == "workspace_ready")
                .count(),
            1
        );
        assert_eq!(client.workspace(&record.id).await.unwrap().status, "ready");
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn schema_four_migration_preserves_node_sessions_and_original_receipts() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let store = open(data.clone()).await;
        let client = store.client();
        let node = client.node().await.unwrap();
        let request = CreateSessionRequest {
            command_id: "legacy".into(),
            provider: "opencode-go".into(),
            model: "glm-5.3-flash".into(),
            ..Default::default()
        };
        let receipt = client.create_session(request.clone()).await.unwrap();
        let session = client.session(&receipt.session_id).await.unwrap();
        client.submit(|connection| {
            connection.execute_batch("DROP TABLE task_events; DROP TABLE runs; DROP TABLE tasks; DROP TABLE batches; ALTER TABLE sessions DROP COLUMN managed_workspace_id; DROP TABLE project_events; DROP TABLE managed_workspaces; DROP TABLE projects; PRAGMA user_version=4;").map_err(|_| StoreError::Database)
        }).await.unwrap();
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let client = store.client();
        assert_eq!(client.node().await.unwrap(), node);
        assert_eq!(client.session(&session.id).await.unwrap(), session);
        assert_eq!(client.create_session(request).await.unwrap(), receipt);
        assert!(client.projects(None, 50).await.unwrap().items.is_empty());
        store.shutdown().await.unwrap();
    }
}
