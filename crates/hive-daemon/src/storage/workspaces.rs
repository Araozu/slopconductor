//! One directory registry, independent of sessions. Worktree creation is explicit.
use super::*;
use hive_protocol::workspaces::*;
use hive_runtime::{
    git::{GitError, Repository},
    tools::WorkspacePolicy,
};

const MAX_WORKSPACES: i64 = 256;
const COLUMNS: &str = "id,name,owner_node_id,path,git_common_dir,managed_worktree,source_workspace_id,base_commit,status,error_code,effects_unknown,is_default,color";

#[derive(Clone)]
pub(super) struct ResolvedWorkspace {
    pub(super) workspace: WorkspaceResponse,
}
pub(super) fn migrate_legacy(tx: &Transaction<'_>) -> StoreResult<()> {
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

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("CREATE TABLE workspaces (
        created_order INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,
        name TEXT NOT NULL,owner_node_id TEXT NOT NULL,path TEXT UNIQUE,
        git_common_dir TEXT,managed_worktree INTEGER NOT NULL DEFAULT 0,
        source_workspace_id TEXT REFERENCES workspaces(id),base_commit TEXT,
        status TEXT NOT NULL,error_code TEXT,effects_unknown INTEGER NOT NULL DEFAULT 0,is_default INTEGER NOT NULL DEFAULT 0,
        CHECK ((is_default=1 AND id='default' AND path IS NULL AND status='ready' AND managed_worktree=0)
            OR (is_default=0 AND id!='default' AND path IS NOT NULL)));
        CREATE TABLE workspace_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id),kind TEXT NOT NULL);
        CREATE INDEX workspace_event_owner ON workspace_events(workspace_id,sequence);
        INSERT INTO workspaces(id,name,owner_node_id,path,git_common_dir,status)
            SELECT id,name,owner_node_id,path,CASE WHEN git_common_dir LIKE 'folder:%' THEN NULL ELSE git_common_dir END,'ready' FROM projects;
        INSERT INTO workspaces(id,name,owner_node_id,path,git_common_dir,managed_worktree,source_workspace_id,base_commit,status,error_code,effects_unknown)
            SELECT w.id,w.name,w.owner_node_id,w.path,p.git_common_dir,1,w.project_id,w.base_commit,
                CASE WHEN w.status='reserved' THEN 'failed' ELSE w.status END,
                CASE WHEN w.status='reserved' THEN 'worktree_not_created' ELSE w.error_code END,w.effects_unknown
            FROM managed_workspaces w JOIN projects p ON p.id=w.project_id;
        INSERT INTO workspaces(id,name,owner_node_id,path,status)
            SELECT 'directory-'||id,'Opened directory',owner_node_id,workspace_root,'ready' FROM sessions
            WHERE workspace_root IS NOT NULL AND workspace_root NOT IN(SELECT path FROM workspaces) GROUP BY workspace_root;
        INSERT INTO workspaces(id,name,owner_node_id,status,is_default) SELECT 'default','No workspace',node_id,'ready',1 FROM node_identity;
        ALTER TABLE sessions ADD COLUMN workspace_id TEXT NOT NULL DEFAULT 'default' REFERENCES workspaces(id);
        UPDATE sessions SET workspace_id=COALESCE((SELECT id FROM workspaces WHERE path=sessions.workspace_root),'default');
        UPDATE sessions SET spec=json_set(json_remove(spec,'$.project'),'$.workspace',
            json_object('workspace_id',workspace_id,'allowed_tools',COALESCE(json_extract(execution,'$.allowed_tools'),json('[]')),'limits',CASE WHEN execution IS NULL THEN NULL ELSE json_remove(execution,'$.root','$.allowed_tools') END)),
            requested_spec=json_set(json_remove(requested_spec,'$.project'),'$.workspace',
            json_object('workspace_id',workspace_id,'allowed_tools',COALESCE(json_extract(execution,'$.allowed_tools'),json('[]')),'limits',CASE WHEN execution IS NULL THEN NULL ELSE json_remove(execution,'$.root','$.allowed_tools') END));
        UPDATE batches SET spec=json_set(json_remove(spec,'$.project'),'$.workspace',
            CASE WHEN json_extract(spec,'$.project.project_id') IS NULL THEN json_object('workspace_id','default','allowed_tools',json('[]'))
            ELSE json_object('workspace_id',json_extract(spec,'$.project.project_id'),'allowed_tools',json_extract(spec,'$.project.allowed_tools')) END);
        UPDATE conversation_drafts SET draft=json_set(json_remove(draft,'$.project','$.allowed_tools'),'$.workspace',
            CASE WHEN json_type(draft,'$.workspace')='object' THEN json_extract(draft,'$.workspace')
            WHEN json_extract(draft,'$.project.project_id') IS NOT NULL THEN json_object('workspace_id',json_extract(draft,'$.project.project_id'),'allowed_tools',COALESCE(json_extract(draft,'$.project.allowed_tools'),json('[]')))
            WHEN EXISTS(SELECT 1 FROM workspaces WHERE path=json_extract(draft,'$.workspace')) THEN
                json_object('workspace_id',(SELECT id FROM workspaces WHERE path=json_extract(draft,'$.workspace')),'allowed_tools',COALESCE(json_extract(draft,'$.allowed_tools'),json('[]')))
            ELSE NULL END);
        INSERT INTO workspace_events(workspace_id,kind) SELECT COALESCE(workspace_id,project_id),kind FROM project_events ORDER BY sequence;
        ALTER TABLE sessions DROP COLUMN managed_workspace_id;
        DROP TABLE project_events; DROP TABLE managed_workspaces; DROP TABLE projects;")
        .map_err(|_| StoreError::Database)
}

pub(super) fn migrate_colors(tx: &Transaction<'_>) -> StoreResult<()> {
    let has_color: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('workspaces') WHERE name='color')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if !has_color {
        tx.execute_batch("ALTER TABLE workspaces ADD COLUMN color TEXT NOT NULL DEFAULT '#808080'
            CHECK(length(color)=7 AND substr(color,1,1)='#' AND substr(color,2) NOT GLOB '*[^0-9A-F]*');")
            .map_err(|_| StoreError::Database)?;
    }
    Ok(())
}

fn workspace_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceResponse> {
    Ok(WorkspaceResponse {
        id: row.get(0)?,
        name: row.get(1)?,
        owner_node_id: row.get(2)?,
        path: row.get(3)?,
        git_common_dir: row.get(4)?,
        managed_worktree: row.get(5)?,
        source_workspace_id: row.get(6)?,
        base_commit: row.get(7)?,
        status: row.get(8)?,
        error_code: row.get(9)?,
        effects_unknown: row.get(10)?,
        is_default: row.get(11)?,
        color: row.get(12)?,
    })
}
pub(super) fn workspace(c: &Connection, id: &str) -> StoreResult<WorkspaceResponse> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM workspaces WHERE id=?1"),
        [id],
        workspace_row,
    )
    .optional()
    .map_err(|_| StoreError::Database)?
    .ok_or(StoreError::NotFound)
}
pub(super) fn repository(w: &WorkspaceResponse) -> StoreResult<Repository> {
    Ok(Repository {
        path: w
            .path
            .clone()
            .ok_or(StoreError::Unsupported("workspace.directory"))?,
        common_dir: w
            .git_common_dir
            .clone()
            .ok_or(StoreError::Unsupported("workspace.git"))?,
    })
}
pub(super) fn prior<T: serde::de::DeserializeOwned>(
    c: &Connection,
    command: &str,
    scope: &str,
    payload: &str,
) -> StoreResult<Option<T>> {
    if !valid_command_id(command) {
        return Err(StoreError::Invalid);
    }
    let saved: Option<(String, String, String)> = c
        .query_row(
            "SELECT scope,payload,receipt FROM commands WHERE command_id=?1",
            [command],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    match saved {
        None => Ok(None),
        Some((s, p, r)) if s == scope && p == payload => Ok(Some(
            serde_json::from_str(&r).map_err(|_| StoreError::Database)?,
        )),
        Some(_) => Err(StoreError::Conflict),
    }
}
pub(super) fn save<T: serde::Serialize>(
    c: &Connection,
    command: &str,
    scope: &str,
    payload: &str,
    result: &T,
) -> StoreResult<()> {
    c.execute(
        "INSERT INTO commands(command_id,scope,payload,receipt) VALUES(?1,?2,?3,?4)",
        rusqlite::params![command, scope, payload, canonical(result)?],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}
fn event(c: &Connection, id: &str, kind: &str) -> StoreResult<()> {
    c.execute(
        "INSERT INTO workspace_events(workspace_id,kind) VALUES(?1,?2)",
        rusqlite::params![id, kind],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}
fn transition(
    c: &Connection,
    w: &WorkspaceResponse,
    state: &str,
    kind: &str,
    error: Option<&str>,
    unknown: bool,
) -> StoreResult<()> {
    c.execute(
        "UPDATE workspaces SET status=?1,error_code=?2,effects_unknown=?3 WHERE id=?4",
        rusqlite::params![state, error, unknown, w.id],
    )
    .map_err(|_| StoreError::Database)?;
    event(c, &w.id, kind)
}
pub(super) fn require_available(c: &Connection, s: &SessionResponse) -> StoreResult<()> {
    require_ready(&workspace(c, &s.workspace_id)?)
}
fn require_ready(w: &WorkspaceResponse) -> StoreResult<()> {
    if w.status != "ready" {
        return Err(StoreError::Preflight(
            "workspace_unavailable",
            "The workspace is not ready; inspect its record before continuing.",
        ));
    }
    Ok(())
}
pub(super) fn recover(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut stmt = tx
        .prepare(&format!(
            "SELECT {COLUMNS} FROM workspaces WHERE status IN('allocating','removing')"
        ))
        .map_err(|_| StoreError::Database)?;
    let records = stmt
        .query_map([], workspace_row)
        .map_err(|_| StoreError::Database)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StoreError::Database)?;
    for w in records {
        transition(
            tx,
            &w,
            "failed",
            "workspace_failed",
            Some("daemon_restarted"),
            true,
        )?;
    }
    Ok(())
}
pub(super) fn selection_policy(
    c: &Connection,
    selection: &WorkspaceSelection,
    resolved: &ResolvedWorkspace,
) -> StoreResult<Option<WorkspacePolicy>> {
    let current = workspace(c, &selection.workspace_id)?;
    require_ready(&current)?;
    if current != resolved.workspace {
        return Err(StoreError::Conflict);
    }
    if current.is_default {
        if !selection.allowed_tools.is_empty() || selection.limits.is_some() {
            return Err(StoreError::Invalid);
        }
        return Ok(None);
    }
    if selection.allowed_tools.is_empty() && selection.limits.is_none() {
        return Ok(None);
    }
    let limits = selection.limits.clone().unwrap_or_default();
    let policy = WorkspacePolicy {
        root: current.path.ok_or(StoreError::Database)?,
        allowed_tools: selection.allowed_tools.clone(),
        shell_timeout_ms: limits.shell_timeout_ms,
        max_output_bytes: limits.max_output_bytes,
        max_tool_calls: limits.max_tool_calls,
        max_model_requests: limits.max_model_requests,
    };
    policy.validate().map_err(|_| StoreError::Invalid)?;
    Ok(Some(policy))
}
fn busy(c: &Connection, path: &str, queued: bool) -> StoreResult<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM turns t JOIN sessions s ON s.id=t.session_id WHERE s.workspace_root=?1 AND (t.status='running' OR (?2 AND t.status IN('queued','paused','awaiting_children'))))",rusqlite::params![path,queued],|r|r.get(0)).map_err(|_|StoreError::Database)
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control)
}

impl StoreClient {
    async fn canonical_workspace_folder(&self, path: &str) -> StoreResult<String> {
        if !Path::new(path).is_absolute() {
            return Err(StoreError::Invalid);
        }
        let path = tokio::fs::canonicalize(path)
            .await
            .map_err(|_| StoreError::Invalid)?;
        if !path.is_dir() {
            return Err(StoreError::Invalid);
        }
        path.to_str().map(str::to_owned).ok_or(StoreError::Invalid)
    }
    pub async fn open_workspace(
        &self,
        request: OpenWorkspaceRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let payload = canonical(&request)?;
        let command = request.command_id.clone();
        let check = payload.clone();
        if let Some(saved) = self
            .submit(move |c| prior(c, &command, "open_workspace", &check))
            .await?
        {
            return Ok(saved);
        }
        let path = self.canonical_workspace_folder(&request.path).await?;
        let git_common_dir = if Path::new(&path).join(".git").exists() {
            Some(self.shared.git.inspect(&path).await?.common_dir)
        } else {
            None
        };
        self.submit(move|c|{
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            if let Some(saved)=prior(&tx,&request.command_id,"open_workspace",&payload)? {return Ok(saved)}
            let existing=tx.query_row(&format!("SELECT {COLUMNS} FROM workspaces WHERE path=?1"),[&path],workspace_row).optional().map_err(|_|StoreError::Database)?;
            let w=if let Some(w)=existing {
                if w.status=="removed" && !w.managed_worktree {
                    let count:i64=tx.query_row("SELECT COUNT(*) FROM workspaces WHERE status!='removed'",[],|r|r.get(0)).map_err(|_|StoreError::Database)?;
                    if count>=MAX_WORKSPACES {return Err(StoreError::Limit)}
                    tx.execute("UPDATE workspaces SET status='ready',git_common_dir=?1,error_code=NULL,effects_unknown=0 WHERE id=?2",rusqlite::params![git_common_dir,w.id]).map_err(|_|StoreError::Database)?;
                    event(&tx,&w.id,"workspace_opened")?; workspace(&tx,&w.id)?
                } else {require_ready(&w)?; w}
            } else {
                let count:i64=tx.query_row("SELECT COUNT(*) FROM workspaces WHERE status!='removed'",[],|r|r.get(0)).map_err(|_|StoreError::Database)?;
                if count>=MAX_WORKSPACES {return Err(StoreError::Limit)}
                let id=opaque_id()?;
                let name=Path::new(&path).file_name().and_then(|s|s.to_str()).filter(|s|valid_name(s)).unwrap_or(&id).to_owned();
                tx.execute("INSERT INTO workspaces(id,name,owner_node_id,path,git_common_dir,status) VALUES(?1,?2,?3,?4,?5,'ready')",rusqlite::params![id,name,query_node(&tx)?.node_id,path,git_common_dir]).map_err(|_|StoreError::Database)?;
                event(&tx,&id,"workspace_opened")?; workspace(&tx,&id)?
            };
            save(&tx,&request.command_id,"open_workspace",&payload,&w)?;
            tx.commit().map_err(|_|StoreError::Database)?; Ok(w)
        }).await
    }
    /// Reserve capacity and a durable receipt before creating a directory.
    pub async fn create_workspace(
        &self,
        request: CreateWorkspaceRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let mut jobs = self.shared.workspace_jobs.lock().await;
        while jobs.try_join_next().is_some() {}
        let payload = canonical(&request)?;
        let command = request.command_id.clone();
        let check = payload.clone();
        if let Some(saved) = self
            .submit(move |c| prior(c, &command, "create_workspace", &check))
            .await?
        {
            return Ok(saved);
        }
        if jobs.len() >= 2 {
            return Err(StoreError::Busy);
        }
        let mut components = Path::new(&request.name).components();
        if request.name.is_empty()
            || request.name.contains(['/', '\\'])
            || !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(StoreError::Invalid);
        }
        let parent = self.canonical_workspace_folder(&request.parent).await?;
        let path = Path::new(&parent).join(&request.name);
        if tokio::fs::symlink_metadata(&path).await.is_ok() {
            return Err(StoreError::Conflict);
        }
        let path = path.to_str().ok_or(StoreError::Invalid)?.to_owned();
        let name = request.name.clone();
        let name_for_job = name.clone();
        let initialize_git = request.git;
        let (record, launch) = self.submit(move |c| {
            let tx = c.unchecked_transaction().map_err(|_| StoreError::Database)?;
            if let Some(saved) = prior(&tx, &request.command_id, "create_workspace", &payload)? {
                return Ok((saved, false));
            }
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM workspaces WHERE status!='removed'", [], |row| row.get(0)).map_err(|_| StoreError::Database)?;
            if count >= MAX_WORKSPACES { return Err(StoreError::Limit); }
            let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM workspaces WHERE path=?1)", [&path], |row| row.get(0)).map_err(|_| StoreError::Database)?;
            if existing { return Err(StoreError::Conflict); }
            let id = opaque_id()?;
            tx.execute("INSERT INTO workspaces(id,name,owner_node_id,path,status) VALUES(?1,?2,?3,?4,'allocating')", rusqlite::params![id,name,query_node(&tx)?.node_id,path]).map_err(|_| StoreError::Database)?;
            event(&tx, &id, "workspace_allocating")?;
            let record = workspace(&tx, &id)?;
            save(&tx, &request.command_id, "create_workspace", &payload, &record)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok((record, true))
        }).await?;
        if launch {
            let store = self.clone();
            let saved = record.clone();
            jobs.spawn(async move {
                let result = async {
                    if initialize_git {
                        let path = store
                            .shared
                            .git
                            .create_repository(&parent, &name_for_job)
                            .await?;
                        Ok(Some(store.shared.git.inspect(&path).await?.common_dir))
                    } else {
                        tokio::fs::create_dir(saved.path.as_ref().unwrap())
                            .await
                            .map_err(|_| GitError {
                                code: "workspace_path_denied",
                                effects_unknown: false,
                            })?;
                        Ok(None)
                    }
                }
                .await;
                let (error, common_dir) = match result {
                    Ok(common) => (None, common),
                    Err(error) => (Some(error), None),
                };
                let _ = store
                    .persist_workspace_result(
                        &saved,
                        if error.is_some() { "failed" } else { "ready" },
                        if error.is_some() {
                            "workspace_failed"
                        } else {
                            "workspace_ready"
                        },
                        error,
                        common_dir,
                    )
                    .await;
            });
        }
        Ok(record)
    }
    pub async fn workspace(&self, id: &str) -> StoreResult<WorkspaceResponse> {
        let id = id.to_owned();
        self.submit(move |c| workspace(c, &id)).await
    }
    pub async fn workspaces(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<WorkspaceResponse>> {
        let limit = page_limit(limit)?;
        self.submit(move|c|{
            let mut stmt=c.prepare(&format!("SELECT {COLUMNS},created_order FROM workspaces WHERE created_order>?1 ORDER BY created_order LIMIT ?2")).map_err(|_|StoreError::Database)?;
            let mut rows=stmt.query(rusqlite::params![after.unwrap_or(0).min(i64::MAX as u64) as i64,(limit+1) as i64]).map_err(|_|StoreError::Database)?;
            let mut items=Vec::new(); let mut cursor=0;
            while let Some(row)=rows.next().map_err(|_|StoreError::Database)? {if items.len()==limit {return Ok(Page{items,next_after:Some(cursor)})} cursor=row.get::<_,i64>(13).map_err(|_|StoreError::Database)? as u64;items.push(workspace_row(row).map_err(|_|StoreError::Database)?)}
            Ok(Page{items,next_after:None})
        }).await
    }
    pub async fn workspace_events(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<WorkspaceEventResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move|c|{
            workspace(c,&id)?;
            let mut stmt=c.prepare("SELECT sequence,workspace_id,kind FROM workspace_events WHERE workspace_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(|_|StoreError::Database)?;
            let mut rows=stmt.query(rusqlite::params![id,after.unwrap_or(0).min(i64::MAX as u64) as i64,(limit+1) as i64]).map_err(|_|StoreError::Database)?;
            let mut items=Vec::new();let mut cursor=0;
            while let Some(row)=rows.next().map_err(|_|StoreError::Database)? {if items.len()==limit {return Ok(Page{items,next_after:Some(cursor)})} cursor=row.get::<_,i64>(0).map_err(|_|StoreError::Database)? as u64;items.push(WorkspaceEventResponse{sequence:cursor,workspace_id:row.get(1).map_err(|_|StoreError::Database)?,kind:row.get(2).map_err(|_|StoreError::Database)?})}
            Ok(Page{items,next_after:None})
        }).await
    }
    pub async fn rename_workspace(
        &self,
        id: &str,
        request: RenameWorkspaceRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let id = id.to_owned();
        let payload = canonical(&request)?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope = format!("workspace:{id}:rename");
            if let Some(saved) = prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(saved);
            }
            let w = workspace(&tx, &id)?;
            if w.is_default || matches!(w.status.as_str(), "removed" | "removing") {
                return Err(StoreError::Conflict);
            }
            if !valid_name(&request.name) {
                return Err(StoreError::Invalid);
            }
            tx.execute(
                "UPDATE workspaces SET name=?1 WHERE id=?2",
                rusqlite::params![request.name.trim(), id],
            )
            .map_err(|_| StoreError::Database)?;
            event(&tx, &id, "workspace_renamed")?;
            let w = workspace(&tx, &id)?;
            save(&tx, &request.command_id, &scope, &payload, &w)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(w)
        })
        .await
    }
    pub async fn set_workspace_color(
        &self,
        id: &str,
        request: SetWorkspaceColorRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let id = id.to_owned();
        let payload = canonical(&request)?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope = format!("workspace:{id}:color");
            if let Some(saved) = prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(saved);
            }
            let w = workspace(&tx, &id)?;
            if matches!(w.status.as_str(), "removed" | "removing") {
                return Err(StoreError::Conflict);
            }
            if !(request.color.len() == 7
                && request.color.starts_with('#')
                && request.color.as_bytes()[1..]
                    .iter()
                    .all(u8::is_ascii_hexdigit))
            {
                return Err(StoreError::Invalid);
            }
            tx.execute(
                "UPDATE workspaces SET color=?1 WHERE id=?2",
                rusqlite::params![request.color.to_ascii_uppercase(), id],
            )
            .map_err(|_| StoreError::Database)?;
            event(&tx, &id, "workspace_color_changed")?;
            let w = workspace(&tx, &id)?;
            save(&tx, &request.command_id, &scope, &payload, &w)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(w)
        })
        .await
    }
    pub(super) async fn resolve_job_workspace(
        &self,
        selection: &WorkspaceSelection,
    ) -> StoreResult<Option<ResolvedWorkspace>> {
        if !self.shared.workspace_healthy.load(Ordering::Acquire) {
            return Err(StoreError::Unavailable);
        }
        let s = selection;
        let w = self.workspace(&s.workspace_id).await?;
        require_ready(&w)?;
        if let Some(path) = &w.path
            && self.canonical_workspace_folder(path).await? != *path
        {
            return Err(StoreError::Conflict);
        }
        Ok(Some(ResolvedWorkspace { workspace: w }))
    }
    pub(super) async fn resolve_workspace(
        &self,
        request: &CreateSessionRequest,
    ) -> StoreResult<Option<ResolvedWorkspace>> {
        let command = request.command_id.clone();
        let payload = canonical(request)?;
        if self
            .submit(move |c| prior_receipt(c, &command, "create_session", &payload))
            .await?
            .is_some()
        {
            return Ok(None);
        }
        self.resolve_job_workspace(&request.workspace).await
    }
    /// Admission validates identity only. It never allocates a worktree.
    pub(super) async fn prepare_workspace(
        &self,
        turn: &str,
        _shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> StoreResult<Option<(&'static str, &'static str)>> {
        let turn = turn.to_owned();
        let w=self.submit(move|c|{
            let id:String=c.query_row("SELECT s.workspace_id FROM turns t JOIN sessions s ON s.id=t.session_id WHERE t.id=?1",[turn],|r|r.get(0)).map_err(|_|StoreError::Database)?;
            workspace(c,&id)
        }).await?;
        if w.is_default {
            return Ok(None);
        }
        let result = async {
            require_ready(&w)?;
            if let Some(path) = &w.path
                && self.canonical_workspace_folder(path).await? != *path
            {
                return Err(StoreError::Conflict);
            }
            if w.git_common_dir.is_some() {
                let repo = repository(&w)?;
                let found = self
                    .shared
                    .git
                    .inspect(w.path.as_deref().ok_or(StoreError::Database)?)
                    .await?;
                if Some(found.path) != w.path || found.common_dir != repo.common_dir {
                    return Err(StoreError::Conflict);
                }
            }
            Ok(())
        }
        .await;
        Ok(result.err().map(|_| {
            (
                "workspace_unavailable",
                "The workspace directory or Git identity changed; inspect it before continuing.",
            )
        }))
    }
    pub async fn create_worktree(
        &self,
        id: &str,
        request: CreateWorktreeRequest,
    ) -> StoreResult<WorkspaceResponse> {
        let mut jobs = self.shared.workspace_jobs.lock().await;
        while jobs.try_join_next().is_some() {}
        let scope = format!("workspace:{id}:worktree");
        let payload = canonical(&request)?;
        let check_scope = scope.clone();
        let check_payload = payload.clone();
        let command = request.command_id.clone();
        if let Some(saved) = self
            .submit(move |c| prior(c, &command, &check_scope, &check_payload))
            .await?
        {
            return Ok(saved);
        }
        if jobs.len() >= 2 {
            return Err(StoreError::Busy);
        }
        if request.name.as_ref().is_some_and(|n| !valid_name(n)) {
            return Err(StoreError::Invalid);
        }
        let source = self.workspace(id).await?;
        require_ready(&source)?;
        let repo = repository(&source)?;
        let commit = self
            .shared
            .git
            .resolve(&repo, request.base_ref.as_deref())
            .await?;
        let new_id = opaque_id()?;
        let destination = self
            .shared
            .data_dir
            .join("workspaces")
            .join(&new_id)
            .to_str()
            .ok_or(StoreError::Invalid)?
            .to_owned();
        let (w,launch)=self.submit(move|c|{
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            if let Some(saved)=prior(&tx,&request.command_id,&scope,&payload)? {return Ok((saved,false))}
            if workspace(&tx,&source.id)?!=source {return Err(StoreError::Conflict)}
            let count:i64=tx.query_row("SELECT COUNT(*) FROM workspaces WHERE status!='removed'",[],|r|r.get(0)).map_err(|_|StoreError::Database)?;
            if count>=MAX_WORKSPACES {return Err(StoreError::Limit)}
            tx.execute("INSERT INTO workspaces(id,name,owner_node_id,path,git_common_dir,managed_worktree,source_workspace_id,base_commit,status) VALUES(?1,?2,?3,?4,?5,1,?6,?7,'allocating')",rusqlite::params![new_id,request.name.unwrap_or_else(||new_id.clone()),source.owner_node_id,destination,source.git_common_dir,source.id,commit]).map_err(|_|StoreError::Database)?;
            event(&tx,&new_id,"workspace_allocating")?;
            let w=workspace(&tx,&new_id)?;save(&tx,&request.command_id,&scope,&payload,&w)?;
            tx.commit().map_err(|_|StoreError::Database)?;Ok((w,true))
        }).await?;
        if launch {
            let store = self.clone();
            let record = w.clone();
            jobs.spawn(async move {
                let (_sender, mut cancel) = tokio::sync::watch::channel(false);
                let error = store
                    .shared
                    .git
                    .allocate(
                        &repo,
                        record.path.as_deref().unwrap(),
                        record.base_commit.as_deref().unwrap(),
                        &mut cancel,
                    )
                    .await
                    .err();
                let _ = store
                    .persist_workspace_outcome(
                        &record,
                        if error.is_some() { "failed" } else { "ready" },
                        if error.is_some() {
                            "workspace_failed"
                        } else {
                            "workspace_ready"
                        },
                        error,
                    )
                    .await;
            });
        }
        Ok(w)
    }
    async fn cleanup_repository(&self, w: &WorkspaceResponse) -> StoreResult<Repository> {
        let mut source = w.clone();
        // Worktree chains terminate in a directory opened in place, which Hive never deletes.
        for _ in 0..MAX_WORKSPACES {
            if !source.managed_worktree {
                return repository(&source);
            }
            source = self
                .workspace(
                    source
                        .source_workspace_id
                        .as_deref()
                        .ok_or(StoreError::Database)?,
                )
                .await?;
        }
        Err(StoreError::Database)
    }
    pub async fn workspace_diff(&self, id: &str) -> StoreResult<WorkspaceDiffResponse> {
        let w = self.workspace(id).await?;
        if !matches!(w.status.as_str(), "ready" | "failed") {
            return Err(StoreError::Conflict);
        }
        let path = w
            .path
            .clone()
            .ok_or(StoreError::Unsupported("workspace.directory"))?;
        if self.submit(move |c| busy(c, &path, false)).await? {
            return Err(StoreError::Conflict);
        }
        let repo = repository(&w)?;
        let base = if let Some(base) = w.base_commit {
            base
        } else {
            self.shared.git.resolve(&repo, None).await?
        };
        let diff = self
            .shared
            .git
            .diff(&repo, w.path.as_deref().ok_or(StoreError::Database)?, &base)
            .await?;
        Ok(WorkspaceDiffResponse {
            workspace_id: w.id,
            base_commit: base,
            head_commit: diff.head_commit,
            patch: diff.patch,
            status: diff.status,
        })
    }
    /// Ordinary removal unregisters metadata; only Hive-created worktrees permit filesystem cleanup.
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
        let (w, launch) = self
            .submit(move |c| {
                let tx = c
                    .unchecked_transaction()
                    .map_err(|_| StoreError::Database)?;
                if let Some(saved) = prior(&tx, &request.command_id, &scope, &payload)? {
                    return Ok((saved, false));
                }
                let w = workspace(&tx, &id)?;
                if busy(&tx, w.path.as_deref().unwrap_or(""), true)?
                    || !matches!(w.status.as_str(), "ready" | "removed")
                {
                    return Err(StoreError::Conflict);
                }
                if w.is_default {
                    return Err(StoreError::Conflict);
                }
                let launch = w.managed_worktree && w.status == "ready";
                if launch && !capacity {
                    return Err(StoreError::Busy);
                }
                if w.status != "removed" {
                    transition(
                        &tx,
                        &w,
                        if launch { "removing" } else { "removed" },
                        if launch {
                            "workspace_removing"
                        } else {
                            "workspace_removed"
                        },
                        None,
                        false,
                    )?;
                }
                let w = workspace(&tx, &id)?;
                save(&tx, &request.command_id, &scope, &payload, &w)?;
                tx.commit().map_err(|_| StoreError::Database)?;
                Ok((w, launch))
            })
            .await?;
        if launch {
            let store = self.clone();
            let record = w.clone();
            jobs.spawn(async move {
                let result = async {
                    let repo = store.cleanup_repository(&record).await?;
                    let (_sender, mut cancel) = tokio::sync::watch::channel(false);
                    store
                        .shared
                        .git
                        .remove(
                            &repo,
                            record.path.as_deref().unwrap(),
                            record.base_commit.as_deref().ok_or(StoreError::Database)?,
                            &mut cancel,
                        )
                        .await
                        .map_err(StoreError::from)
                }
                .await;
                let error = match result {
                    Ok(()) => None,
                    Err(StoreError::Git(e)) => Some(e),
                    Err(_) => Some(GitError {
                        code: "storage_unavailable",
                        effects_unknown: false,
                    }),
                };
                let state = match &error {
                    None => "removed",
                    Some(e) if !e.effects_unknown => "ready",
                    Some(_) => "failed",
                };
                let _ = store
                    .persist_workspace_outcome(
                        &record,
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
        Ok(w)
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
        self.persist_workspace_result(record, state, kind, error, None)
            .await
    }
    async fn persist_workspace_result(
        &self,
        record: &WorkspaceResponse,
        state: &'static str,
        kind: &'static str,
        error: Option<GitError>,
        common_dir: Option<String>,
    ) -> StoreResult<()> {
        for _ in 0..50 {
            let record = record.clone();
            let error = error.clone();
            let common_dir = common_dir.clone();
            if self
                .submit(move |c| {
                    let tx = c
                        .unchecked_transaction()
                        .map_err(|_| StoreError::Database)?;
                    let current = workspace(&tx, &record.id)?;
                    let code = error.as_ref().map(|e| e.code);
                    let unknown = error.as_ref().is_some_and(|e| e.effects_unknown);
                    if current.status == state
                        && current.error_code.as_deref() == code
                        && current.effects_unknown == unknown
                    {
                        return Ok(());
                    }
                    if let Some(common_dir) = &common_dir {
                        tx.execute(
                            "UPDATE workspaces SET git_common_dir=?1 WHERE id=?2",
                            rusqlite::params![common_dir, record.id],
                        )
                        .map_err(|_| StoreError::Database)?;
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

/// Build pre-unification tables for historical migration tests only.
#[cfg(test)]
pub(super) fn legacy_fixture(c: &Connection) -> StoreResult<()> {
    c.pragma_update(None, "foreign_keys", "OFF")
        .map_err(|_| StoreError::Database)?;
    let result = (|| {
        let tx = c
            .unchecked_transaction()
            .map_err(|_| StoreError::Database)?;
        migrate_legacy(&tx)?;
        tx.execute_batch("ALTER TABLE projects ADD COLUMN name TEXT NOT NULL DEFAULT '';
            ALTER TABLE managed_workspaces ADD COLUMN name TEXT NOT NULL DEFAULT '';
            INSERT INTO projects(id,name,owner_node_id,path,git_common_dir)
                SELECT id,name,owner_node_id,path,COALESCE(git_common_dir,'folder:'||path) FROM workspaces WHERE is_default=0 AND managed_worktree=0;
            INSERT INTO managed_workspaces(id,name,project_id,session_id,owner_node_id,path,base_commit,status,error_code,effects_unknown)
                SELECT w.id,w.name,w.source_workspace_id,(SELECT id FROM sessions WHERE workspace_id=w.id ORDER BY created_order LIMIT 1),w.owner_node_id,w.path,w.base_commit,w.status,w.error_code,w.effects_unknown
                FROM workspaces w WHERE managed_worktree=1 AND EXISTS(SELECT 1 FROM sessions WHERE workspace_id=w.id);
            UPDATE sessions SET managed_workspace_id=(SELECT id FROM managed_workspaces WHERE id=sessions.workspace_id);
            ALTER TABLE sessions DROP COLUMN last_user_activity_at;
            ALTER TABLE sessions DROP COLUMN workspace_id;
            DROP TABLE workspace_events; DROP TABLE workspaces;").map_err(|_|StoreError::Database)?;
        tx.commit().map_err(|_| StoreError::Database)
    })();
    c.pragma_update(None, "foreign_keys", "ON")
        .map_err(|_| StoreError::Database)?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn open(path: PathBuf) -> Store {
        Store::open(path, None, 64, Duration::from_secs(1))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn colors_validate_persist_and_replay_without_duplicate_events() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let w = c
            .open_workspace(OpenWorkspaceRequest {
                command_id: "open-color".into(),
                path: temp.path().to_str().unwrap().into(),
            })
            .await
            .unwrap();
        assert_eq!(w.color, "#808080");
        for id in [&w.id, DEFAULT_WORKSPACE_ID] {
            for (index, color) in [
                "",
                "red",
                "123456",
                "#abc",
                "#12345678",
                "#gg0000",
                " #123456",
                "#12345é",
            ]
            .iter()
            .enumerate()
            {
                assert!(matches!(
                    c.set_workspace_color(
                        id,
                        SetWorkspaceColorRequest {
                            command_id: format!("invalid-{index}-{id}"),
                            color: (*color).into(),
                        }
                    )
                    .await,
                    Err(StoreError::Invalid)
                ));
            }
            let request = SetWorkspaceColorRequest {
                command_id: format!("color-{id}"),
                color: "#aB12ef".into(),
            };
            let accepted = c.set_workspace_color(id, request.clone()).await.unwrap();
            assert_eq!(accepted.color, "#AB12EF");
            assert_eq!(
                c.set_workspace_color(id, request.clone()).await.unwrap(),
                accepted
            );
            let changed = SetWorkspaceColorRequest {
                color: "#000000".into(),
                ..request
            };
            assert!(matches!(
                c.set_workspace_color(id, changed).await,
                Err(StoreError::Conflict)
            ));
            assert_eq!(
                c.workspace_events(id, None, 200)
                    .await
                    .unwrap()
                    .items
                    .iter()
                    .filter(|e| e.kind == "workspace_color_changed")
                    .count(),
                1
            );
        }
        assert!(
            c.workspaces(None, 200)
                .await
                .unwrap()
                .items
                .iter()
                .all(|w| w.color == "#AB12EF")
        );
        store.shutdown().await.unwrap();
        let store = open(data).await;
        assert_eq!(
            store.client().workspace(&w.id).await.unwrap().color,
            "#AB12EF"
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn version_23_workspaces_gain_default_colors() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = open(data.clone()).await;
        store
            .client()
            .submit(|db| {
                db.execute_batch(
                    "ALTER TABLE sessions DROP COLUMN last_user_activity_at; ALTER TABLE workspaces DROP COLUMN color; PRAGMA user_version=23;",
                )
                .map_err(|_| StoreError::Database)
            })
            .await
            .unwrap();
        store.shutdown().await.unwrap();
        let store = open(data).await;
        assert_eq!(
            store
                .client()
                .workspace(DEFAULT_WORKSPACE_ID)
                .await
                .unwrap()
                .color,
            "#808080"
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn directory_creation_reserves_capacity_and_replays_its_original_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let store = open(temp.path().join("data")).await;
        let c = store.client();
        let request = CreateWorkspaceRequest {
            command_id: "create-folder".into(),
            parent: temp.path().to_str().unwrap().into(),
            name: "new-folder".into(),
            git: false,
        };
        let accepted = c.create_workspace(request.clone()).await.unwrap();
        assert_eq!(accepted.status, "allocating");
        assert!(!accepted.managed_worktree);
        c.drain_workspace_jobs().await;
        let ready = c.workspace(&accepted.id).await.unwrap();
        assert_eq!(ready.status, "ready");
        assert!(Path::new(ready.path.as_ref().unwrap()).is_dir());
        assert_eq!(c.create_workspace(request.clone()).await.unwrap(), accepted);
        let mut changed = request;
        changed.git = true;
        assert!(matches!(
            c.create_workspace(changed).await,
            Err(StoreError::Conflict)
        ));
        c.submit(|db|{
            db.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<254) INSERT INTO workspaces(id,name,owner_node_id,path,status) SELECT 'capacity-'||i,'Capacity',?1,'/unavailable/'||i,'ready' FROM n",[query_node(db)?.node_id]).map_err(|_|StoreError::Database)?;Ok(())
        }).await.unwrap();
        let denied = CreateWorkspaceRequest {
            command_id: "capacity-denied".into(),
            parent: temp.path().to_str().unwrap().into(),
            name: "must-not-exist".into(),
            git: false,
        };
        assert!(matches!(
            c.create_workspace(denied).await,
            Err(StoreError::Limit)
        ));
        assert!(!temp.path().join("must-not-exist").exists());
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn schema_twenty_one_retains_directories_worktrees_and_default_membership() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let root = temp.path().join("directory");
        fs::create_dir(&root).unwrap();
        let checkout = temp.path().join("worktree");
        fs::create_dir(&checkout).unwrap();
        let checkout = fs::canonicalize(checkout).unwrap();
        let store = open(data.clone()).await;
        let c = store.client();
        let directory = c
            .open_workspace(OpenWorkspaceRequest {
                command_id: "migration-open".into(),
                path: root.to_str().unwrap().into(),
            })
            .await
            .unwrap();
        let directory_id = directory.id.clone();
        let worktree = seed(&c, checkout.to_str().unwrap().into(), "ready").await;
        let worktree_id = worktree.id.clone();
        let query_id = worktree_id.clone();
        let source = directory_id.clone();
        c.submit(move |db| {
            db.execute(
                "UPDATE workspaces SET git_common_dir='/unavailable/.git' WHERE id=?1",
                [&source],
            )
            .map_err(|_| StoreError::Database)?;
            db.execute(
                "UPDATE workspaces SET source_workspace_id=?1 WHERE id=?2",
                rusqlite::params![source, query_id],
            )
            .map_err(|_| StoreError::Database)?;
            Ok(())
        })
        .await
        .unwrap();
        let mut sessions = Vec::new();
        for (command, workspace_id, allowed_tools) in [
            ("default", DEFAULT_WORKSPACE_ID.to_owned(), vec![]),
            ("directory", directory_id.clone(), vec!["read".into()]),
            ("worktree", worktree_id.clone(), vec!["edit".into()]),
        ] {
            let mut request = super::super::tests::create_request(command, None);
            request.workspace = WorkspaceSelection {
                workspace_id,
                allowed_tools,
                limits: None,
            };
            sessions.push(c.create_session(request).await.unwrap().session_id);
        }
        store.shutdown().await.unwrap();
        let db = Connection::open(data.join(DATABASE_FILE)).unwrap();
        legacy_fixture(&db).unwrap();
        db.pragma_update(None, "user_version", 21).unwrap();
        drop(db);
        let store = open(data).await;
        let c = store.client();
        for (session, id) in
            sessions
                .iter()
                .zip([DEFAULT_WORKSPACE_ID, &directory_id, &worktree_id])
        {
            let thread = c.session(session).await.unwrap();
            assert_eq!(thread.workspace_id, id);
            assert_eq!(thread.spec.workspace.workspace_id, id);
        }
        let recovered = c.workspace(&worktree_id).await.unwrap();
        assert!(recovered.managed_worktree);
        assert_eq!(recovered.source_workspace_id, Some(directory_id));
        assert_eq!(recovered.base_commit, Some("a".repeat(40)));
        assert_eq!(recovered.path, worktree.path);
        assert_eq!(recovered.status, "ready");
        assert!(c.workspace(DEFAULT_WORKSPACE_ID).await.unwrap().is_default);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn every_session_has_a_workspace_and_directory_opening_is_in_place() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("keep.txt"), "existing contents").unwrap();
        let data = temp.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let default = c.workspace(DEFAULT_WORKSPACE_ID).await.unwrap();
        assert!(default.is_default);
        assert_eq!(default.path, None);
        let plain = c
            .create_session(super::super::tests::create_request("plain", None))
            .await
            .unwrap();
        assert_eq!(
            c.session(&plain.session_id).await.unwrap().workspace_id,
            default.id
        );
        let payload = OpenWorkspaceRequest {
            command_id: "open".into(),
            path: folder.to_str().unwrap().into(),
        };
        let w = c.open_workspace(payload.clone()).await.unwrap();
        assert!(!w.managed_worktree);
        assert_eq!(
            w.path,
            Some(fs::canonicalize(&folder).unwrap().to_str().unwrap().into())
        );
        assert_eq!(c.open_workspace(payload).await.unwrap(), w);
        for command in ["one", "two"] {
            let mut request = super::super::tests::create_request(command, None);
            request.workspace = WorkspaceSelection {
                workspace_id: w.id.clone(),
                allowed_tools: vec!["read".into()],
                limits: None,
            };
            let session = c.create_session(request).await.unwrap();
            assert_eq!(
                c.session(&session.session_id).await.unwrap().workspace_id,
                w.id
            );
        }
        assert_eq!(c.workspaces(None, 200).await.unwrap().items.len(), 2);
        assert!(!data.join("workspaces").exists());
        c.remove_workspace(
            &w.id,
            RemoveWorkspaceRequest {
                command_id: "close".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read_to_string(folder.join("keep.txt")).unwrap(),
            "existing contents"
        );
        assert!(matches!(
            c.remove_workspace(
                DEFAULT_WORKSPACE_ID,
                RemoveWorkspaceRequest {
                    command_id: "default-remove".into()
                }
            )
            .await,
            Err(StoreError::Conflict)
        ));
        store.shutdown().await.unwrap();
    }

    async fn seed(c: &StoreClient, path: String, state: &str) -> WorkspaceResponse {
        let state = state.to_owned();
        c.submit(move|db|{
            let id=opaque_id()?;
            db.execute("INSERT INTO workspaces(id,name,owner_node_id,path,git_common_dir,managed_worktree,source_workspace_id,base_commit,status) VALUES(?1,?1,?2,?3,'/unavailable/.git',1,'default',?4,?5)",rusqlite::params![id,query_node(db)?.node_id,path,"a".repeat(40),state]).map_err(|_|StoreError::Database)?;
            workspace(db,&id)
        }).await.unwrap()
    }

    #[tokio::test]
    async fn interrupted_git_operations_fail_without_replaying_or_touching_directories() {
        let temp = tempfile::tempdir().unwrap();
        let data = temp.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let allocating = seed(
            &c,
            temp.path().join("allocating").to_str().unwrap().into(),
            "allocating",
        )
        .await;
        let removing = seed(
            &c,
            temp.path().join("removing").to_str().unwrap().into(),
            "removing",
        )
        .await;
        fs::create_dir(removing.path.as_ref().unwrap()).unwrap();
        fs::write(
            Path::new(removing.path.as_ref().unwrap()).join("keep"),
            "retain",
        )
        .unwrap();
        store.shutdown().await.unwrap();
        let store = open(data.clone()).await;
        let c = store.client();
        for prior in [&allocating, &removing] {
            let w = c.workspace(&prior.id).await.unwrap();
            assert_eq!(w.status, "failed");
            assert!(w.effects_unknown);
            assert_eq!(w.error_code.as_deref(), Some("daemon_restarted"));
            let mut request =
                super::super::tests::create_request(&format!("session-{}", w.id), None);
            request.workspace = WorkspaceSelection {
                workspace_id: w.id.clone(),
                allowed_tools: vec!["read".into()],
                limits: None,
            };
            assert!(matches!(
                c.create_session(request).await,
                Err(StoreError::Preflight("workspace_unavailable", _))
            ));
            assert_eq!(
                c.workspace_events(&w.id, None, 200)
                    .await
                    .unwrap()
                    .items
                    .len(),
                1
            );
        }
        assert!(!Path::new(allocating.path.as_ref().unwrap()).exists());
        assert_eq!(
            fs::read_to_string(Path::new(removing.path.as_ref().unwrap()).join("keep")).unwrap(),
            "retain"
        );
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let c = store.client();
        assert_eq!(
            c.workspace_events(&allocating.id, None, 200)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn outcome_write_retries_commit_state_and_event_once() {
        let temp = tempfile::tempdir().unwrap();
        let store = open(temp.path().join("data")).await;
        let c = store.client();
        let record = seed(
            &c,
            temp.path().join("pending").to_str().unwrap().into(),
            "allocating",
        )
        .await;
        c.submit(|db|db.execute_batch("CREATE TRIGGER fail_workspace_event BEFORE INSERT ON workspace_events WHEN NEW.kind='workspace_ready' BEGIN SELECT RAISE(ABORT,'injected'); END;").map_err(|_|StoreError::Database)).await.unwrap();
        let writer = c.clone();
        let saved = record.clone();
        let handle = tokio::spawn(async move {
            writer
                .persist_workspace_outcome(&saved, "ready", "workspace_ready", None)
                .await
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!handle.is_finished());
        assert_eq!(c.workspace(&record.id).await.unwrap().status, "allocating");
        c.submit(|db| {
            db.execute_batch("DROP TRIGGER fail_workspace_event")
                .map_err(|_| StoreError::Database)
        })
        .await
        .unwrap();
        handle.await.unwrap().unwrap();
        c.persist_workspace_outcome(&record, "ready", "workspace_ready", None)
            .await
            .unwrap();
        assert_eq!(
            c.workspace_events(&record.id, None, 200)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        store.shutdown().await.unwrap();
    }
}
