//! Child acceptance, selected context, dependency waits, and cancellation share
//! the same transactional application operations for API and native tools.
use super::*;
use hive_protocol::orchestration::*;
use hive_runtime::{agent::ToolIntent, orchestration::OrchestrationResult, tools::ToolOutcome};
use std::io::Read;

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE tasks ADD COLUMN root_task_id TEXT;
        ALTER TABLE tasks ADD COLUMN admission_batch_id TEXT REFERENCES batches(id);
        UPDATE tasks SET root_task_id=id,admission_batch_id=batch_id;
        CREATE TABLE child_links(child_task_id TEXT PRIMARY KEY REFERENCES tasks(id),
            parent_task_id TEXT NOT NULL REFERENCES tasks(id),parent_run_id TEXT NOT NULL REFERENCES runs(id),
            root_task_id TEXT NOT NULL REFERENCES tasks(id),depth INTEGER NOT NULL CHECK(depth BETWEEN 1 AND 4),
            cancel_with_parent INTEGER NOT NULL,context TEXT NOT NULL);
        CREATE INDEX children_parent ON child_links(parent_run_id);
        CREATE INDEX children_root ON child_links(root_task_id);
        CREATE TABLE child_waits(id TEXT PRIMARY KEY,parent_turn_id TEXT NOT NULL REFERENCES turns(id),
            invocation_id TEXT UNIQUE REFERENCES tool_invocations(id),child_run_ids TEXT NOT NULL,
            completed INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX waits_parent ON child_waits(parent_turn_id,completed);")
        .map_err(|_|StoreError::Database)
}

pub(super) fn migrate_notifications(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE child_links ADD COLUMN notification_mode TEXT NOT NULL DEFAULT 'manual';
        ALTER TABLE child_links ADD COLUMN detached INTEGER NOT NULL DEFAULT 0;
        CREATE TABLE child_notifications(created_order INTEGER PRIMARY KEY AUTOINCREMENT, child_run_id TEXT NOT NULL UNIQUE REFERENCES turns(id), parent_turn_id TEXT NOT NULL REFERENCES turns(id), parent_session_id TEXT NOT NULL REFERENCES sessions(id), delivered INTEGER NOT NULL DEFAULT 0);
        CREATE INDEX child_notifications_parent ON child_notifications(parent_session_id,delivered);")
        .map_err(|_| StoreError::Database)
}

/// Normalize only old subagent command receipts whose typed DTOs gained
/// defaulted fields. Receipt identity and all semantic request fields remain
/// part of the comparison; unrelated command payloads are never rewritten.
pub(super) fn migrate_subagent_receipt_payloads(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut after = 0_i64;
    loop {
        // Page by immutable row identity. Historical payloads stay on disk;
        // release the read cursor before updating each bounded page.
        let mut stmt=tx.prepare("SELECT rowid,command_id,scope,payload FROM commands WHERE rowid>?1 AND (scope IN('create_task','create_session','create_batch') OR scope LIKE 'run:%:child' OR scope LIKE 'run:%:wait') ORDER BY rowid LIMIT 64").map_err(|_|StoreError::Database)?;
        let mut query = stmt.query([after]).map_err(|_| StoreError::Database)?;
        let mut page = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = query.next().map_err(|_| StoreError::Database)? {
            let payload: String = row.get(3).map_err(|_| StoreError::Database)?;
            if !page.is_empty() && bytes.saturating_add(payload.len()) > 4 * 1024 * 1024 {
                break;
            }
            bytes = bytes.saturating_add(payload.len());
            page.push((
                row.get::<_, i64>(0).map_err(|_| StoreError::Database)?,
                row.get::<_, String>(1).map_err(|_| StoreError::Database)?,
                row.get::<_, String>(2).map_err(|_| StoreError::Database)?,
                payload,
            ));
        }
        drop(query);
        drop(stmt);
        if page.is_empty() {
            break;
        }
        for (rowid, command, scope, payload) in page {
            after = rowid;
            let normalized = match scope.as_str() {
                "create_task" => serde_json::from_str::<CreateTaskRequest>(&payload)
                    .ok()
                    .filter(|r| r.spec.orchestration.is_some())
                    .and_then(|r| canonical(&r).ok()),
                "create_session" => serde_json::from_str::<CreateSessionRequest>(&payload)
                    .ok()
                    .filter(|r| r.orchestration.is_some())
                    .and_then(|r| canonical(&r).ok()),
                "create_batch" => serde_json::from_str::<CreateBatchRequest>(&payload)
                    .ok()
                    .filter(|r| r.spec.orchestration.is_some())
                    .and_then(|r| canonical(&r).ok()),
                _ if scope.ends_with(":child") => {
                    serde_json::from_str::<CreateChildRequest>(&payload)
                        .ok()
                        .and_then(|r| canonical(&r).ok())
                }
                _ if scope.ends_with(":wait") => {
                    serde_json::from_str::<WaitChildrenRequest>(&payload)
                        .ok()
                        .and_then(|r| canonical(&r).ok())
                }
                _ => None,
            };
            if let Some(normalized) = normalized.filter(|value| value != &payload) {
                tx.execute(
                    "UPDATE commands SET payload=?3 WHERE command_id=?1 AND scope=?2 AND payload=?4",
                    rusqlite::params![command, scope, normalized, payload],
                )
                .map_err(|_| StoreError::Database)?;
            }
        }
    }
    Ok(())
}

pub(super) fn migrate_wait_modes(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE child_waits ADD COLUMN mode TEXT NOT NULL DEFAULT 'all';
        ALTER TABLE child_waits ADD COLUMN deadline_ms INTEGER;",
    )
    .map_err(|_| StoreError::Database)
}

pub(super) fn migrate_child_activity(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE child_links ADD COLUMN spawned_at_ms INTEGER;")
        .map_err(|_| StoreError::Database)
}

pub(super) fn migrate_wait_results(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE child_waits ADD COLUMN result TEXT;")
        .map_err(|_| StoreError::Database)
}

pub(super) fn link(c: &Connection, task: &str) -> StoreResult<Option<ChildLink>> {
    c.query_row("SELECT parent_task_id,parent_run_id,root_task_id,depth,cancel_with_parent,context,notification_mode,detached FROM child_links WHERE child_task_id=?1",[task],|r|Ok(ChildLink{parent_task_id:r.get(0)?,parent_run_id:r.get(1)?,root_task_id:r.get(2)?,depth:r.get(3)?,cancel_with_parent:r.get(4)?,context:execution::json_column(r,5)?,notification_mode:r.get(6)?,detached:r.get(7)?})).optional().map_err(|_|StoreError::Database)
}

pub(super) fn check_reactivation_capacity(c: &Connection, child_task: &str) -> StoreResult<()> {
    let Some(link) = link(c, child_task)? else {
        return Ok(());
    };
    let unfinished: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=?1 AND status IN('queued','running','paused','awaiting_children'))",[child_task],|r|r.get(0)).map_err(|_|StoreError::Database)?;
    if unfinished {
        return Ok(());
    }
    let root = orchestration::task(c, &link.root_task_id)?;
    let policy = root
        .spec
        .orchestration
        .as_ref()
        .ok_or(StoreError::Database)?;
    let root_count:u32=c.query_row("SELECT count(*) FROM child_links l WHERE l.root_task_id=?1 AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=l.child_task_id AND t.status IN('queued','running','paused','awaiting_children'))",[&link.root_task_id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
    let direct_count:u32=c.query_row("SELECT count(*) FROM child_links l WHERE l.parent_task_id=?1 AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=l.child_task_id AND t.status IN('queued','running','paused','awaiting_children'))",[&link.parent_task_id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
    if root_count >= policy.max_children || direct_count >= policy.max_children {
        return Err(StoreError::Limit);
    }
    Ok(())
}

pub(crate) fn validate_policy(spec: &SessionSpec) -> StoreResult<()> {
    let Some(policy) = &spec.orchestration else {
        return Ok(());
    };
    let runtime: hive_runtime::orchestration::OrchestrationPolicy =
        serde_json::from_str(&canonical(policy)?).map_err(|_| StoreError::Invalid)?;
    if !hive_runtime::orchestration::validate(&runtime)
        || spec.budget.is_none()
        || policy
            .allowed_tools
            .iter()
            .any(|tool| !spec.workspace.allowed_tools.contains(tool))
    {
        return Err(StoreError::Invalid);
    }
    for model in &policy.allowed_models {
        execution::validate_settings(model, &Default::default())?;
    }
    Ok(())
}

fn parent(c: &Connection, id: &str) -> StoreResult<SessionResponse> {
    let run = orchestration::run(c, id)?;
    let mut task = orchestration::task(c, &run.task_id)?;
    if !matches!(
        run.turn.status.as_str(),
        "queued" | "running" | "paused" | "awaiting_children"
    ) || task.spec.orchestration.is_none()
    {
        return Err(StoreError::Conflict);
    }
    if let Some(batch) = c
        .query_row(
            "SELECT admission_batch_id FROM sessions WHERE id=?1",
            [&task.id],
            |r| r.get::<_, Option<String>>(0),
        )
        .map_err(|_| StoreError::Database)?
        && orchestration::get_batch(c, &batch)?.cancellation_requested
    {
        return Err(StoreError::Conflict);
    }
    if cancellation_requested(c, &run.turn.id)? {
        return Err(StoreError::Conflict);
    }
    // Queued follow-ups may have different settings. Child authority and
    // defaults come from the addressed active turn.
    task.spec.settings = run.turn.settings.clone();
    task.spec.model = run.turn.requested_model.clone();
    task.latest_run = Some(run);
    Ok(task)
}

fn constrain(
    c: &Connection,
    parent: &SessionResponse,
    spec: &mut SessionSpec,
) -> StoreResult<(String, u32)> {
    let policy = parent
        .spec
        .orchestration
        .as_ref()
        .ok_or(StoreError::Conflict)?;
    if !policy.allowed_models.is_empty() && !policy.allowed_models.contains(&spec.model) {
        return Err(StoreError::Unsupported("child.model"));
    }
    // Existing deeper trees remain readable after upgrade, but no child can
    // delegate and no new nested policy can be introduced through API, tools,
    // history forks, or follow-up attempts.
    if link(c, &parent.id)?.is_some() || spec.orchestration.is_some() {
        return Err(StoreError::Unsupported("child.orchestration"));
    }
    let p = &parent.spec.workspace;
    let child = &mut spec.workspace;
    if child.workspace_id != hive_protocol::workspaces::DEFAULT_WORKSPACE_ID {
        let descends: bool = c.query_row("WITH RECURSIVE ancestry(id,source_workspace_id) AS (SELECT id,source_workspace_id FROM workspaces WHERE id=?1 UNION SELECT w.id,w.source_workspace_id FROM workspaces w JOIN ancestry a ON w.id=a.source_workspace_id) SELECT EXISTS(SELECT 1 FROM ancestry WHERE id=?2)",rusqlite::params![child.workspace_id,p.workspace_id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        if !descends
            || child
                .allowed_tools
                .iter()
                .any(|t| !policy.allowed_tools.contains(t) || !p.allowed_tools.contains(t))
        {
            return Err(StoreError::Unsupported("child.workspace"));
        }
        let limits = p.limits.clone().unwrap_or_default();
        let child_limits = child.limits.get_or_insert_with(|| limits.clone());
        if child_limits.shell_timeout_ms > limits.shell_timeout_ms
            || child_limits.max_output_bytes > limits.max_output_bytes
            || child_limits.max_tool_calls > limits.max_tool_calls
            || child_limits.max_model_requests > limits.max_model_requests
        {
            return Err(StoreError::Unsupported("child.workspace.limits"));
        }
    }
    let inherited = parent.spec.budget.as_ref().ok_or(StoreError::Invalid)?;
    if let Some(b) = &spec.budget {
        if b.max_model_requests > inherited.max_model_requests
            || b.max_tool_calls > inherited.max_tool_calls
        {
            return Err(StoreError::Unsupported("child.budget"));
        }
    } else {
        spec.budget = Some(inherited.clone());
    }
    let link = link(c, &parent.id)?;
    let (root, depth) = link.map_or((parent.id.clone(), 1), |l| (l.root_task_id, l.depth + 1));
    let root_task = orchestration::task(c, &root)?;
    let root_policy = root_task
        .spec
        .orchestration
        .as_ref()
        .ok_or(StoreError::Database)?;
    let outstanding: u32 = c
        .query_row(
            "SELECT count(*) FROM child_links l WHERE l.root_task_id=?1 AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=l.child_task_id AND t.status IN('queued','running','paused','awaiting_children'))",
            [&root],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let direct: u32 = c
        .query_row(
            "SELECT count(*) FROM child_links l WHERE l.parent_task_id=?1 AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=l.child_task_id AND t.status IN('queued','running','paused','awaiting_children'))",
            [&parent.id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if depth > root_policy.max_depth
        || outstanding >= root_policy.max_children
        || direct >= policy.max_children
    {
        return Err(StoreError::Limit);
    }
    orchestration::validate_spec(spec)?;
    Ok((root, depth))
}

fn selected_context(
    c: &Connection,
    parent: &SessionResponse,
    selection: &ChildContext,
    prompt: &str,
    artifact_dir: &Path,
) -> StoreResult<String> {
    if selection.message_ids.len() > 16 || selection.artifact_ids.len() > 16 {
        return Err(StoreError::Limit);
    }
    let mut seen = std::collections::HashSet::new();
    let mut text = prompt.to_owned();
    for id in &selection.message_ids {
        if !seen.insert(id) || id.len() > 128 {
            return Err(StoreError::Invalid);
        }
        let (session, status, public): (String, String, String) = c
            .query_row(
                "SELECT session_id,status,text FROM messages WHERE id=?1 AND deleted=0",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)?;
        if session
            != parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .session_id
            || status != "completed"
        {
            return Err(StoreError::Unsupported("child.context"));
        }
        text.push_str(&format!("\n\nSelected public message {id}:\n{public}"));
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(StoreError::Limit);
        }
    }
    for id in &selection.artifact_ids {
        if !seen.insert(id)
            || id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(StoreError::Invalid);
        }
        let owned:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations i JOIN turns t ON t.id=i.turn_id JOIN json_each(i.outcome,'$.artifacts') a WHERE t.session_id=?1 AND json_extract(a.value,'$.id')=?2)",rusqlite::params![parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        if !owned {
            return Err(StoreError::Unsupported("child.artifact"));
        }
        let media: String = c
            .query_row("SELECT media_type FROM artifacts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Database)?;
        if !media.starts_with("text/") && !media.starts_with("application/json") {
            return Err(StoreError::Unsupported("child.artifact_media_type"));
        }
        let mut bytes = Vec::new();
        File::open(artifact_dir.join(id))
            .map_err(|_| StoreError::Unsupported("child.artifact_unavailable"))?
            .take((hive_runtime::tools::PREVIEW_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError::Unsupported("child.artifact_unavailable"))?;
        let public = String::from_utf8_lossy(&bytes);
        let preview = bounded_text(&public, hive_runtime::tools::PREVIEW_BYTES);
        text.push_str(&format!(
            "\n\nSelected parent artifact {id} (bounded public preview):\n{preview}"
        ));
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(StoreError::Limit);
        }
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(StoreError::Limit);
    }
    Ok(text)
}

fn completed(output: String) -> ToolOutcome {
    ToolOutcome {
        status: "completed".into(),
        output,
        artifacts: Vec::new(),
        error_code: None,
        effects_unknown: false,
    }
}
fn bounded_text(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

fn summaries(c: &Connection, ids: &[String]) -> StoreResult<String> {
    summaries_with_limit(c, ids, hive_runtime::tools::PREVIEW_BYTES)
}

fn summaries_with_limit(c: &Connection, ids: &[String], budget: usize) -> StoreResult<String> {
    let limit = (budget / ids.len().max(1)).saturating_sub(400);
    let mut values = Vec::new();
    for id in ids {
        let run = orchestration::run(c, id)?;
        let output = orchestration::output(c, run.turn.assistant_message_id.as_deref())?;
        let text = output.as_ref().map(|m| bounded_text(&m.text, limit));
        values.push(serde_json::json!({"run_id":run.id,"task_id":run.task_id,"status":run.turn.status,"error_code":run.turn.error_code,"effects_unknown":run.effects_unknown,"assistant_message_id":run.turn.assistant_message_id,"text":text,"output_truncated":output.as_ref().is_some_and(|m|m.text.len()>limit)}));
    }
    let mut encoded = canonical(&values)?;
    if encoded.len() > budget {
        for value in &mut values {
            value["text"] = serde_json::Value::Null;
            value["output_truncated"] = serde_json::Value::Bool(true);
        }
        encoded = canonical(&values)?;
    }
    if encoded.len() > budget {
        return Err(StoreError::Limit);
    }
    Ok(encoded)
}

fn wait_summaries(
    c: &Connection,
    ids: &[String],
    mode: &str,
    timed_out: bool,
) -> StoreResult<String> {
    let mut completed_ids = Vec::new();
    let mut pending_ids = Vec::new();
    for id in ids {
        let run = orchestration::run(c, id)?;
        if matches!(
            run.turn.status.as_str(),
            "queued" | "running" | "paused" | "awaiting_children"
        ) {
            pending_ids.push(id.clone());
        } else {
            completed_ids.push(id.clone());
        }
    }
    let mut value = serde_json::json!({"mode":mode,"timed_out":timed_out,"completed_run_ids":completed_ids,"pending_run_ids":pending_ids});
    let wrapper_bytes = canonical(&value)?.len();
    let result_budget = hive_runtime::tools::PREVIEW_BYTES
        .saturating_sub(wrapper_bytes)
        .saturating_sub(16);
    value["results"] =
        serde_json::from_str::<serde_json::Value>(&summaries_with_limit(c, ids, result_budget)?)
            .map_err(|_| StoreError::Database)?;
    let output = canonical(&value)?;
    if output.len() > hive_runtime::tools::PREVIEW_BYTES {
        return Err(StoreError::Limit);
    }
    Ok(output)
}

fn direct_child(
    c: &Connection,
    parent_run: &str,
    child_task: &str,
) -> StoreResult<SessionResponse> {
    let matched: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM child_links WHERE parent_task_id=(SELECT session_id FROM turns WHERE id=?1) AND child_task_id=?2)",
            rusqlite::params![parent_run, child_task],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if !matched {
        return Err(StoreError::NotFound);
    }
    orchestration::task(c, child_task)
}

/// Pick the current executable child attempt even when a later follow-up has
/// already been queued. Once no attempt is unfinished, expose the latest
/// terminal attempt for result and progress inspection.
fn current_child_run(c: &Connection, child_task: &str) -> StoreResult<RunResponse> {
    let turn: Option<String> = c
        .query_row(
            "SELECT id FROM turns WHERE session_id=?1 AND status IN('queued','running','paused','awaiting_children') ORDER BY ordinal ASC LIMIT 1",
            [child_task],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    let turn = match turn {
        Some(turn) => turn,
        None => c
            .query_row(
                "SELECT id FROM turns WHERE session_id=?1 ORDER BY ordinal DESC LIMIT 1",
                [child_task],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)?,
    };
    orchestration::run(c, &turn)
}

fn unfinished_child_turns(c: &Connection, child_task: &str) -> StoreResult<Vec<String>> {
    let mut stmt = c
        .prepare("SELECT id FROM turns WHERE session_id=?1 AND status IN('queued','running','paused','awaiting_children') ORDER BY ordinal ASC")
        .map_err(|_| StoreError::Database)?;
    stmt.query_map([child_task], |r| r.get(0))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)
}

fn accept_wait(
    tx: &Transaction<'_>,
    parent_run: &str,
    request: &WaitChildrenRequest,
    intent: Option<&ToolIntent>,
) -> StoreResult<()> {
    let parent = parent(tx, parent_run)?;
    if !matches!(request.mode.as_str(), "all" | "any")
        || request
            .timeout_ms
            .is_some_and(|timeout| timeout == 0 || timeout > 86_400_000)
    {
        return Err(StoreError::Invalid);
    }
    // Recover an accepted native wait before resolving a fresh "all" snapshot
    // or computing a new deadline. The original selection and deadline win.
    let existing: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT parent_turn_id,invocation_id,mode FROM child_waits WHERE id=?1",
            [&request.command_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    if let Some((turn, invocation, mode)) = existing {
        if turn
            != parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .turn
                .id
            || invocation.as_deref() != intent.map(|i| i.id.as_str())
            || mode != request.mode
        {
            return Err(StoreError::Conflict);
        }
        return wake(tx);
    }
    let mut selected_ids = request.child_run_ids.clone();
    if selected_ids.is_empty() {
        let mut stmt = tx.prepare("SELECT COALESCE((SELECT r.id FROM turns r WHERE r.session_id=l.child_task_id AND r.status IN('queued','running','paused','awaiting_children') ORDER BY r.ordinal ASC LIMIT 1),(SELECT r.id FROM turns r WHERE r.session_id=l.child_task_id ORDER BY r.ordinal DESC LIMIT 1)) FROM child_links l WHERE l.parent_task_id=?1 AND EXISTS(SELECT 1 FROM turns x WHERE x.session_id=l.child_task_id) ORDER BY l.rowid LIMIT 33").map_err(|_| StoreError::Database)?;
        selected_ids = stmt
            .query_map([&parent.id], |r| r.get::<_, String>(0))
            .map_err(|_| StoreError::Database)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| StoreError::Database)?;
    }
    if selected_ids.len() > 32
        || selected_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != selected_ids.len()
    {
        return Err(StoreError::Invalid);
    }
    for id in &selected_ids {
        let run = orchestration::run(tx, id)?;
        direct_child(tx, parent_run, &run.task_id)?;
    }
    let encoded = canonical(&selected_ids)?;
    let deadline_ms = request.timeout_ms.map(|timeout| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .saturating_add(timeout as u128)
            .min(i64::MAX as u128) as i64
    });
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM child_waits WHERE parent_turn_id=?1 AND completed=0)",
            [&parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .turn
                .id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let total: u32 = tx
        .query_row(
            "SELECT count(*) FROM child_waits WHERE parent_turn_id=?1",
            [&parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .turn
                .id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if pending || total >= 64 {
        return Err(StoreError::Conflict);
    }
    tx.execute("INSERT INTO child_waits(id,parent_turn_id,invocation_id,child_run_ids,mode,deadline_ms) VALUES(?1,?2,?3,?4,?5,?6)",rusqlite::params![request.command_id,parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.turn.id,intent.map(|i|&i.id),encoded,request.mode,deadline_ms]).map_err(|_|StoreError::Database)?;
    if let Some(intent) = intent {
        tx.execute(
            "UPDATE tool_invocations SET status='waiting' WHERE id=?1",
            [&intent.id],
        )
        .map_err(|_| StoreError::Database)?;
    }
    append_event(
        tx,
        &parent
            .latest_run
            .as_ref()
            .ok_or(StoreError::Conflict)?
            .session_id,
        "child_wait_accepted",
        Some(
            &parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .turn
                .id,
        ),
        None,
    )?;
    wake(tx)?;
    Ok(())
}

pub(super) fn pending(c: &Connection, turn: &str) -> StoreResult<bool> {
    c.query_row(
        "SELECT EXISTS(SELECT 1 FROM child_waits WHERE parent_turn_id=?1 AND completed=0)",
        [turn],
        |r| r.get(0),
    )
    .map_err(|_| StoreError::Database)
}

pub(super) fn attached_pending(c: &Connection, turn: &str) -> StoreResult<bool> {
    c.query_row("SELECT EXISTS(SELECT 1 FROM child_links l WHERE l.parent_run_id=?1 AND l.detached=0 AND EXISTS(SELECT 1 FROM turns t WHERE t.session_id=l.child_task_id AND t.status IN('queued','running','paused','awaiting_children')))", [turn], |r| r.get(0)).map_err(|_| StoreError::Database)
}

/// Record the outcome alongside its terminal transition, without scanning history.
pub(super) fn enqueue_completion(tx: &Transaction<'_>, turn: &str) -> StoreResult<()> {
    tx.execute(
        "INSERT OR IGNORE INTO child_notifications(child_run_id,parent_turn_id,parent_session_id)
        SELECT r.id,l.parent_run_id,l.parent_task_id FROM turns r JOIN child_links l ON l.child_task_id=r.session_id
        WHERE r.id=?1 AND l.notification_mode='completion'
        AND r.status IN('completed','failed','cancelled','interrupted','incomplete')
        AND r.history_only=0
        AND NOT EXISTS(SELECT 1 FROM child_notifications n WHERE n.child_run_id=r.id)",
        [turn],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

/// A completed child only wakes a known continuation. Never dispatch a tool here.
pub(super) fn wake(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut stmt=tx.prepare("SELECT id,parent_turn_id,invocation_id,child_run_ids,mode,deadline_ms FROM child_waits WHERE completed=0").map_err(|_|StoreError::Database)?;
    let waits = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                execution::json_column::<Vec<String>>(r, 3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for (id, turn, invocation, ids, mode, deadline) in waits {
        let encoded = canonical(&ids)?;
        let all:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM json_each(?1) j JOIN turns r ON r.id=j.value WHERE r.status IN('queued','running','paused','awaiting_children'))",[&encoded],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        let any:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?1) j JOIN turns r ON r.id=j.value WHERE r.status IN('completed','failed','cancelled','interrupted','incomplete'))",[&encoded],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let timed_out = deadline.is_some_and(|deadline| deadline <= now);
        let done = ids.is_empty() || timed_out || (mode == "all" && all) || (mode == "any" && any);
        if !done {
            continue;
        }
        let session: String = tx
            .query_row("SELECT session_id FROM turns WHERE id=?1", [&turn], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Database)?;
        let result = wait_summaries(tx, &ids, &mode, timed_out)?;
        if let Some(invocation) = invocation {
            let intent = tool_intent(tx, &invocation)?;
            execution::finish_tool_tx(tx, &session, &turn, &intent, &completed(result.clone()))?;
        }
        tx.execute(
            "UPDATE child_waits SET completed=1,result=?2 WHERE id=?1",
            rusqlite::params![id, result],
        )
        .map_err(|_| StoreError::Database)?;
        tx.execute(
            "UPDATE turns SET status='queued' WHERE id=?1 AND status='awaiting_children'",
            [&turn],
        )
        .map_err(|_| StoreError::Database)?;
        append_event(tx, &session, "child_wait_completed", Some(&turn), None)?;
    }
    let mut stmt = tx
        .prepare("SELECT id FROM turns WHERE status='awaiting_children'")
        .map_err(|_| StoreError::Database)?;
    let parked = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for turn in parked {
        let session: String = tx
            .query_row("SELECT session_id FROM turns WHERE id=?1", [&turn], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Database)?;
        let notices: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM child_notifications WHERE parent_session_id=?1 AND delivered=0)", [&session], |r| r.get(0)).map_err(|_| StoreError::Database)?;
        if !pending(tx, &turn)? && (!attached_pending(tx, &turn)? || notices) {
            tx.execute("UPDATE turns SET status='queued' WHERE id=?1 AND status='awaiting_children' AND pause_requested=0 AND cancellation_requested=0", [&turn]).map_err(|_| StoreError::Database)?;
            append_event(tx, &session, "child_wait_completed", Some(&turn), None)?;
        }
    }
    Ok(())
}

pub(super) fn deliver_notifications(tx: &Transaction<'_>, turn: &str) -> StoreResult<bool> {
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    let mut stmt = tx.prepare("SELECT n.child_run_id,l.child_task_id,r.status,r.error_code,EXISTS(SELECT 1 FROM tool_invocations i WHERE i.turn_id=r.id AND json_extract(i.outcome,'$.effects_unknown')=1),m.id,m.text FROM child_notifications n JOIN turns r ON r.id=n.child_run_id JOIN child_links l ON l.child_task_id=r.session_id LEFT JOIN messages m ON m.id=r.assistant_message_id WHERE n.parent_session_id=?2 AND n.delivered=0 ORDER BY n.created_order LIMIT 8").map_err(|_| StoreError::Database)?;
    let notices = stmt
        .query_map(rusqlite::params![turn, session], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    if notices.is_empty() {
        return Ok(false);
    }
    let model_ref = messages::turn_model(tx, turn)?;
    let mut preview_limit = 768usize;
    let text = loop {
        let mut entries = Vec::new();
        for (run, task_id, status, error, effects_unknown, message_id, output) in &notices {
            let preview = bounded_text(output.as_deref().unwrap_or(""), preview_limit);
            entries.push(serde_json::json!({"run_id":run,"task_id":task_id,"status":status,"error_code":error,"effects_unknown":effects_unknown,"assistant_message_id":message_id,"text":preview,"output_truncated":output.as_ref().is_some_and(|text|text.len()>preview.len())}));
        }
        let text = format!(
            "Child task completion notices (public results only): {}",
            canonical(&entries)?
        );
        if text.len() <= hive_runtime::tools::PREVIEW_BYTES {
            break text;
        }
        if preview_limit == 0 {
            return Err(StoreError::Limit);
        }
        preview_limit /= 2;
    };
    let ordinal = execution::next_message_ordinal(tx, turn)?;
    tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,provider,model) VALUES(?1,?2,?3,?4,'user',?5,'completed',?6,?7)", rusqlite::params![opaque_id()?,session,turn,ordinal,text,model_ref.provider().as_str(),model_ref.model()]).map_err(|_| StoreError::Database)?;
    for (run, ..) in notices {
        tx.execute(
            "UPDATE child_notifications SET delivered=1 WHERE child_run_id=?1",
            [run],
        )
        .map_err(|_| StoreError::Database)?;
    }
    append_event(tx, &session, "child_completion_delivered", Some(turn), None)?;
    Ok(true)
}

pub(super) fn park(tx: &Transaction<'_>, turn: &str) -> StoreResult<()> {
    let status = if pending(tx, turn)? || attached_pending(tx, turn)? {
        "awaiting_children"
    } else {
        "queued"
    };
    tx.execute(
        "UPDATE turns SET status=?1,pause_requested=0 WHERE id=?2",
        rusqlite::params![status, turn],
    )
    .map_err(|_| StoreError::Database)?;
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    append_event(tx, &session, "turn_awaiting_children", Some(turn), None)?;
    Ok(())
}

fn tool_intent(c: &Connection, id: &str) -> StoreResult<ToolIntent> {
    c.query_row("SELECT id,request_id,result_message_id,call_id,provider_call_id,name,arguments FROM tool_invocations WHERE id=?1",[id],|r|Ok(ToolIntent{id:r.get(0)?,request_id:r.get(1)?,result_message_id:r.get(2)?,call_id:r.get(3)?,provider_call_id:r.get(4)?,name:r.get(5)?,arguments:execution::json_column(r,6)?})).map_err(|_|StoreError::Database)
}

pub(super) fn cancel_descendants(tx: &Transaction<'_>, parent_turn: &str) -> StoreResult<()> {
    tx.execute("UPDATE message_queue SET status='cancelled' WHERE status='pending' AND session_id IN(SELECT child_task_id FROM child_links WHERE parent_run_id=?1 AND cancel_with_parent=1)", [parent_turn]).map_err(|_|StoreError::Database)?;
    let mut stmt=tx.prepare("SELECT t.id FROM child_links l JOIN turns parent ON parent.id=l.parent_run_id JOIN turns r ON r.session_id=l.child_task_id JOIN turns t ON t.id=r.id WHERE parent.id=?1 AND l.cancel_with_parent=1 AND t.status IN('queued','running','paused','awaiting_children')").map_err(|_|StoreError::Database)?;
    let ids = stmt
        .query_map([parent_turn], |r| r.get::<_, String>(0))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for id in ids {
        cancel_turn_tx(
            tx,
            &id,
            CancelTurnRequest {
                command_id: opaque_id()?,
            },
        )?;
    }
    Ok(())
}

pub(super) fn retry_allowed(c: &Connection, task: &SessionResponse) -> StoreResult<()> {
    let mut current = task.id.clone();
    while let Some(link) = link(c, &current)? {
        if !link.cancel_with_parent {
            break;
        }
        let parent = orchestration::run(c, &link.parent_run_id)?;
        if parent.turn.status == "cancelled" || cancellation_requested(c, &parent.turn.id)? {
            return Err(StoreError::Conflict);
        }
        current = link.parent_task_id;
    }
    let batch: Option<String> = c
        .query_row(
            "SELECT admission_batch_id FROM sessions WHERE id=?1",
            [&task.id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if let Some(batch) = batch
        && orchestration::get_batch(c, &batch)?.cancellation_requested
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

pub(super) fn recover_waits(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut stmt=tx.prepare("SELECT id,session_id FROM turns WHERE status='awaiting_children' OR (status='running' AND EXISTS(SELECT 1 FROM child_waits w WHERE w.parent_turn_id=turns.id AND w.completed=0))").map_err(|_|StoreError::Database)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for (turn, session) in rows {
        tx.execute("UPDATE turns SET status='paused' WHERE id=?1", [&turn])
            .map_err(|_| StoreError::Database)?;
        append_event(tx, &session, "turn_wait_interrupted", Some(&turn), None)?;
    }
    wake(tx)
}

impl StoreClient {
    pub async fn create_child(
        &self,
        parent_run: &str,
        request: CreateChildRequest,
        effective: Option<SessionSpec>,
    ) -> StoreResult<TaskReceipt> {
        self.create_child_inner(parent_run, request, effective, None)
            .await
    }

    async fn create_child_inner(
        &self,
        parent_run: &str,
        request: CreateChildRequest,
        effective: Option<SessionSpec>,
        intent: Option<ToolIntent>,
    ) -> StoreResult<TaskReceipt> {
        if !valid_command_id(&request.command_id) {
            return Err(StoreError::Invalid);
        }
        if !matches!(request.notification_mode.as_str(), "completion" | "manual") {
            return Err(StoreError::Invalid);
        }
        let scope = format!("run:{parent_run}:child");
        let payload = canonical(&request)?;
        if let Some(receipt) = self
            .prior_job(&request.command_id, &scope, &payload)
            .await?
        {
            return Ok(receipt);
        }
        let parent_id = parent_run.to_owned();
        let lookup = parent_id.clone();
        self.submit(move |c| parent(c, &lookup)).await?;
        let mut spec = effective.ok_or(StoreError::Invalid)?;
        let resolved = self.resolve_job_workspace(&spec.workspace).await?;
        let artifact_dir = self.shared.data_dir.join("artifacts");
        self.submit(move|c| {
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            if let Some(receipt)=workspaces::prior(&tx,&request.command_id,&scope,&payload)? {return Ok(receipt);}
            let parent=parent(&tx,&parent_id)?;
            let (root,depth)=constrain(&tx,&parent,&mut spec)?;
            spec.prompt=selected_context(&tx,&parent,&request.context,&spec.prompt,&artifact_dir)?;
            let receipt=orchestration::insert_task(&tx,&spec,&request.spec,resolved,None,&request.command_id)?;
            // Agent-authored child prompts are not human interaction.
            tx.execute("UPDATE sessions SET last_user_activity_at=NULL WHERE id=?1", [&receipt.session_id]).map_err(|_|StoreError::Database)?;
            let spawned_at_ms=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().min(i64::MAX as u128) as i64;
            tx.execute("INSERT INTO child_links(child_task_id,parent_task_id,parent_run_id,root_task_id,depth,cancel_with_parent,context,notification_mode,detached,spawned_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",rusqlite::params![receipt.task_id,parent.id,parent_id,root,depth,request.cancel_with_parent,canonical(&request.context)?,request.notification_mode,request.detached,spawned_at_ms]).map_err(|_|StoreError::Database)?;
            tx.execute("UPDATE sessions SET root_task_id=?1,admission_batch_id=(SELECT admission_batch_id FROM sessions WHERE id=?2) WHERE id=?3",rusqlite::params![root,parent.id,receipt.task_id]).map_err(|_|StoreError::Database)?;
            append_event(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,"child_created",Some(&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.turn.id),None)?;
            workspaces::save(&tx,&request.command_id,&scope,&payload,&receipt)?;
            if let Some(intent)=intent {execution::finish_tool_tx(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.turn.id,&intent,&completed(canonical(&receipt)?))?;}
            tx.commit().map_err(|_|StoreError::Database)?;
            Ok(receipt)
        }).await
    }

    pub async fn children(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<SessionResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move|c| {orchestration::run(c,&id)?;orchestration::page(c,"SELECT k.id,k.created_order FROM sessions k JOIN child_links l ON l.child_task_id=k.id WHERE l.parent_task_id=(SELECT session_id FROM turns WHERE id=?1) AND k.created_order>?2 ORDER BY k.created_order LIMIT ?3",Some(&id),after,limit,orchestration::task)}).await
    }

    pub async fn run_result(&self, id: &str) -> StoreResult<RunResultResponse> {
        let id = id.to_owned();
        self.submit(move |c| {
            let run = orchestration::run(c, &id)?;
            let output = orchestration::output(c, run.turn.assistant_message_id.as_deref())?;
            Ok(RunResultResponse { run, output })
        })
        .await
    }

    pub async fn wait_children(
        &self,
        id: &str,
        request: WaitChildrenRequest,
    ) -> StoreResult<CommandReceipt> {
        if !valid_command_id(&request.command_id) {
            return Err(StoreError::Invalid);
        }
        let id = id.to_owned();
        let scope = format!("run:{id}:wait");
        let payload = canonical(&request)?;
        self.submit(move |c| {
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) = workspaces::prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let parent = parent(&tx, &id)?;
            accept_wait(&tx, &id, &request, None)?;
            if parent
                .latest_run
                .as_ref()
                .ok_or(StoreError::Conflict)?
                .turn
                .status
                == "queued"
                && pending(
                    &tx,
                    &parent
                        .latest_run
                        .as_ref()
                        .ok_or(StoreError::Conflict)?
                        .turn
                        .id,
                )?
            {
                park(
                    &tx,
                    &parent
                        .latest_run
                        .as_ref()
                        .ok_or(StoreError::Conflict)?
                        .turn
                        .id,
                )?;
            }
            let current = get_session(
                &tx,
                &parent
                    .latest_run
                    .as_ref()
                    .ok_or(StoreError::Conflict)?
                    .session_id,
            )?;
            let receipt = make_receipt(
                &request.command_id,
                &current.id,
                Some(
                    parent
                        .latest_run
                        .as_ref()
                        .ok_or(StoreError::Conflict)?
                        .turn
                        .id
                        .clone(),
                ),
                None,
                current.revision,
                current.last_event_sequence,
            );
            workspaces::save(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub(super) async fn native_orchestration(
        &self,
        turn: &str,
        intent: &ToolIntent,
    ) -> StoreResult<OrchestrationResult> {
        let turn_id = turn.to_owned();
        let lookup = turn_id.clone();
        let run_id = self
            .submit(move |c| {
                c.query_row("SELECT id FROM turns WHERE id=?1", [lookup], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(|_| StoreError::Database)
            })
            .await?;
        if intent.name == "child_create" {
            let mut args = intent.arguments.clone();
            args["command_id"] = serde_json::Value::String(format!("child-{}", intent.id));
            let request: CreateChildRequest =
                serde_json::from_value(args).map_err(|_| StoreError::Invalid)?;
            let mut effective = request.spec.clone();
            let active = self.run(&run_id).await?;
            if effective.settings.max_output_tokens.is_none()
                && !effective.model.starts_with("codex/")
            {
                effective.settings.max_output_tokens = active.turn.settings.max_output_tokens;
            }
            let receipt = self
                .create_child_inner(&run_id, request, Some(effective), Some(intent.clone()))
                .await?;
            return Ok(OrchestrationResult::Completed(completed(canonical(
                &receipt,
            )?)));
        }
        let intent = intent.clone();
        self.submit(move|c| {
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            let parent=parent(&tx,&run_id)?;
            if intent.name == "child_discover" {
                let mut models = Vec::new();
                for provider_id in hive_core::provider::ProviderId::all() {
                    if let Some(provider) = hive_runtime::providers::provider(*provider_id) {
                        for model in hive_runtime::providers::catalog::models(provider) {
                            let caps = hive_runtime::providers::inference::capabilities(provider, &model.id);
                            models.push(serde_json::json!({"id":format!("{}/{}",provider_id.as_str(),model.id),"provider":provider_id.as_str(),"model":model.id,"display_name":model.display_name,"adapter_supported":true,"account_entitlement":"not_checked","capabilities":{"tools":caps.tools,"context_window_tokens":caps.context_window_tokens,"max_input_tokens":caps.max_input_tokens,"reasoning_efforts":caps.reasoning_efforts}}));
                        }
                    }
                }
                models.sort_by(|a,b|a["id"].as_str().cmp(&b["id"].as_str()));
                let allowlist = parent.spec.orchestration.as_ref().map(|p| &p.allowed_models);
                if let Some(allowed) = allowlist.filter(|items| !items.is_empty()) {
                    models.retain(|model| model["id"].as_str().is_some_and(|id| allowed.iter().any(|item| item == id)));
                }
                let after = intent.arguments.get("after").and_then(serde_json::Value::as_str).unwrap_or("");
                let limit = intent.arguments.get("limit").and_then(serde_json::Value::as_u64).unwrap_or(20).clamp(1,32) as usize;
                let mut page: Vec<_> = models.into_iter().filter(|m|m["id"].as_str().is_some_and(|id|id>after)).take(limit+1).collect();
                let next_after = if page.len()>limit {page.pop();page.last().and_then(|m|m["id"].as_str().map(str::to_owned))} else {None};
                let output = canonical(&serde_json::json!({"models":page,"next_after":next_after,"credentials_included":false,"entitlement_checked":false}))?;
                let outcome = completed(output);
                execution::finish_tool_tx(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,&turn_id,&intent,&outcome)?;
                tx.commit().map_err(|_|StoreError::Database)?;
                return Ok(OrchestrationResult::Completed(outcome));
            }
            let result=if intent.name=="child_wait" {
                let requested:Vec<String>=intent.arguments.get("child_run_ids").cloned().map(serde_json::from_value).transpose().map_err(|_|StoreError::Invalid)?.unwrap_or_default();
                let mode=intent.arguments.get("mode").and_then(serde_json::Value::as_str).unwrap_or("all").to_owned();
                let timeout_ms=intent.arguments.get("timeout_ms").and_then(serde_json::Value::as_u64);
                let command_id=format!("wait-{}",intent.id);
                accept_wait(&tx,&run_id,&WaitChildrenRequest{command_id:command_id.clone(),child_run_ids:requested,mode,timeout_ms},Some(&intent))?;
                let (ids,mode,deadline,saved_result):(Vec<String>,String,Option<i64>,Option<String>)=tx.query_row("SELECT child_run_ids,mode,deadline_ms,result FROM child_waits WHERE id=?1",[&command_id],|r|Ok((execution::json_column(r,0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|_|StoreError::Database)?;
                if pending(&tx,&turn_id)? {OrchestrationResult::Waiting} else {
                    let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as i64;
                    let output=match saved_result {Some(output)=>output,None=>wait_summaries(&tx,&ids,&mode,deadline.is_some_and(|d|d<=now))?};
                    OrchestrationResult::Completed(completed(output))
                }
            } else {
                let task_id=intent.arguments["task_id"].as_str().ok_or(StoreError::Invalid)?;
                let child=direct_child(&tx,&run_id,task_id)?;
                if intent.name == "child_send" {
                    let delivery = match intent.arguments["delivery"].as_str().unwrap_or("next-boundary") {
                        "next-boundary" => DeliveryMode::NextBoundary,
                        "after-turn" => DeliveryMode::AfterTurn,
                        "after-finish" => DeliveryMode::AfterFinish,
                        _ => return Err(StoreError::Invalid),
                    };
                    let request = SendMessageRequest {
                        command_id: format!("child-send-{}", intent.id),
                        text: intent.arguments["text"].as_str().ok_or(StoreError::Invalid)?.to_owned(),
                        delivery: Some(delivery),
                        ..Default::default()
                    };
                    let scope = format!("session:{}:message", child.id);
                    let payload = canonical(&request)?;
                    if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
                        let outcome = completed(canonical(&receipt)?);
                        execution::finish_tool_tx(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,&turn_id,&intent,&outcome)?;
                        tx.commit().map_err(|_|StoreError::Database)?;
                        return Ok(OrchestrationResult::Completed(outcome));
                    }
                    let current = current_child_run(&tx, &child.id)?;
                    if cancellation_requested(&tx, &current.turn.id)?
                        || matches!(current.turn.status.as_str(), "cancelled" | "interrupted" | "failed")
                    {
                        return Err(StoreError::Conflict);
                    }
                    let receipt = messages::send_direct_tx(&tx, &child.id, request, None, None, None, false)?;
                    let outcome=completed(canonical(&receipt)?);
                    execution::finish_tool_tx(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,&turn_id,&intent,&outcome)?;
                    tx.commit().map_err(|_|StoreError::Database)?;
                    return Ok(OrchestrationResult::Completed(outcome));
                }
                let output=match intent.name.as_str() {
                    "child_inspect"=>{
                        let run=current_child_run(&tx,&child.id)?;
                        let (requests,tools,latest_tool):(u32,u32,Option<String>)=tx.query_row("SELECT (SELECT count(*) FROM model_requests WHERE turn_id=?1),(SELECT count(*) FROM tool_invocations WHERE turn_id=?1),(SELECT name FROM tool_invocations WHERE turn_id=?1 ORDER BY rowid DESC LIMIT 1)",[&run.turn.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|StoreError::Database)?;
                        let (activity,sequence):(Option<String>,Option<i64>)=tx.query_row("SELECT kind,sequence FROM session_events WHERE session_id=?1 ORDER BY sequence DESC LIMIT 1",[&run.session_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|_|StoreError::Database)?.unwrap_or((None,None));
                        let event_after=intent.arguments.get("event_after").and_then(serde_json::Value::as_i64).unwrap_or(0);
                        let event_limit=intent.arguments.get("event_limit").and_then(serde_json::Value::as_u64).unwrap_or(8).clamp(1,16) as usize;
                        let mut stmt=tx.prepare("SELECT sequence,kind,turn_id,message_id FROM session_events WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(|_|StoreError::Database)?;
                        let events=stmt.query_map(rusqlite::params![run.session_id,event_after,(event_limit+1) as i64],|r|Ok(serde_json::json!({"sequence":r.get::<_,i64>(0)? as u64,"kind":r.get::<_,String>(1)? ,"turn_id":r.get::<_,Option<String>>(2)? ,"message_id":r.get::<_,Option<String>>(3)?}))).map_err(|_|StoreError::Database)?.collect::<Result<Vec<_>,_>>().map_err(|_|StoreError::Database)?;
                        drop(stmt);
                        let more=events.len()>event_limit;
                        let mut events=events;
                        if more {events.pop();}
                        let next_cursor=if more {events.last().and_then(|event|event["sequence"].as_u64())} else {None};
                        let spawned_at_ms:Option<i64>=tx.query_row("SELECT spawned_at_ms FROM child_links WHERE child_task_id=?1",[&child.id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
                        let elapsed_since_spawn_ms=spawned_at_ms.map(|spawned|{let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis().min(i64::MAX as u128) as i64;now.saturating_sub(spawned)});
                        canonical(&serde_json::json!({"task_id":child.id,"run_id":run.id,"latest_run_id":child.latest_run.as_ref().map(|r|r.id.as_str()),"status":run.turn.status,"workspace_id":run.workspace_id,"model":run.turn.requested_model,"budget_usage":child.budget_usage,"usage":{"model_requests":requests,"tool_calls":tools},"latest_tool":latest_tool,"latest_activity":activity,"event_sequence":sequence,"spawned_at_ms":spawned_at_ms,"elapsed_since_spawn_ms":elapsed_since_spawn_ms,"events":events,"next_event_cursor":next_cursor}))?
                    },
                    "child_result"=>{
                        if !unfinished_child_turns(&tx,&child.id)?.is_empty() {return Err(StoreError::Conflict);}
                        let run=current_child_run(&tx,&child.id)?;
                        summaries(&tx,&[run.id])?
                    },
                    "child_cancel"=>{
                        // The parent tool invocation is the durable aggregate receipt.
                        // Check it before inspecting mutable child status so a retry
                        // after cancellation returns the original result verbatim.
                        let prior: Option<String> = tx.query_row(
                            "SELECT outcome FROM tool_invocations WHERE id=?1 AND turn_id=?2 AND status='completed'",
                            rusqlite::params![intent.id, turn_id],
                            |r| r.get(0),
                        ).optional().map_err(|_|StoreError::Database)?;
                        if let Some(prior) = prior {
                            let outcome: ToolOutcome = serde_json::from_str(&prior).map_err(|_|StoreError::Database)?;
                            tx.commit().map_err(|_|StoreError::Database)?;
                            return Ok(OrchestrationResult::Completed(outcome));
                        }
                        let mut runs=unfinished_child_turns(&tx,&child.id)?;
                        let mut stmt=tx.prepare("SELECT turn_id FROM message_queue WHERE session_id=?1 AND status='pending' ORDER BY sequence").map_err(|_|StoreError::Database)?;
                        let pending=stmt.query_map([&child.id],|r|r.get::<_,String>(0)).map_err(|_|StoreError::Database)?.collect::<rusqlite::Result<Vec<_>>>().map_err(|_|StoreError::Database)?;
                        drop(stmt);
                        // Pending queue IDs come first so no cancelled run can
                        // automatically release its unsent messages.
                        runs.splice(0..0,pending);
                        let total=runs.len();
                        let mut cancelled=Vec::new();
                        for (index, run_id) in runs.iter().enumerate() {
                            cancel_turn_tx(&tx,run_id,CancelTurnRequest{command_id:format!("child-cancel-{}-{index}",intent.id)})?;
                            if cancelled.len()<32 {cancelled.push(run_id.clone());}
                        }
                        canonical(&serde_json::json!({"cancelled_run_ids":cancelled,"cancelled_count":total,"run_ids_truncated":total>cancelled.len()}))?
                    },
                    _=>return Err(StoreError::Invalid),
                };
                let outcome=completed(output);
                execution::finish_tool_tx(&tx,&parent.latest_run.as_ref().ok_or(StoreError::Conflict)?.session_id,&turn_id,&intent,&outcome)?;
                OrchestrationResult::Completed(outcome)
            };
            tx.commit().map_err(|_|StoreError::Database)?;Ok(result)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hive_runtime::{
        agent::{RequestCompletion, RequestIntent},
        chat::{ChatOutcome, ChatRepository, ChatStatus},
        providers::{Usage, inference::*},
    };

    async fn open(data: PathBuf) -> Store {
        Store::open(data, None, 64, Duration::from_secs(1))
            .await
            .unwrap()
    }
    fn spec(prompt: &str) -> SessionSpec {
        SessionSpec {
            title: None,
            prompt: prompt.into(),
            model: "opencode-go/glm-5.3-flash".into(),
            settings: hive_protocol::execution::GenerationSettings {
                max_output_tokens: Some(64),
                reasoning_effort: None,
                full_context: None,
            },
            workspace: Default::default(),
            budget: Some(OperationBudget {
                max_model_requests: 32,
                max_tool_calls: 64,
            }),
            orchestration: None,
        }
    }
    fn policy() -> hive_protocol::orchestration::OrchestrationPolicy {
        hive_protocol::orchestration::OrchestrationPolicy {
            max_children: 4,
            max_concurrent_children: 4,
            max_depth: 2,
            allowed_models: vec!["opencode-go/glm-5.3-flash".into()],
            allowed_tools: vec![],
        }
    }
    async fn root(c: &StoreClient, id: &str) -> TaskReceipt {
        let mut spec = spec(id);
        spec.orchestration = Some(policy());
        c.create_task(
            CreateTaskRequest {
                command_id: id.into(),
                spec: spec.clone(),
            },
            Some(spec),
        )
        .await
        .unwrap()
    }
    fn child(id: &str) -> CreateChildRequest {
        CreateChildRequest {
            command_id: id.into(),
            spec: spec(id),
            context: ChildContext::default(),
            cancel_with_parent: true,
            notification_mode: "completion".into(),
            detached: false,
        }
    }
    fn outcome(code: Option<&str>) -> ChatOutcome {
        ChatOutcome {
            text: "public result".into(),
            resolved_model: None,
            usage: Usage::default(),
            status: ChatStatus::Completed,
            error_code: code.map(str::to_owned),
            error_message: None,
        }
    }
    async fn record(
        c: &StoreClient,
        turn: &str,
        ordinal: u32,
        name: &str,
        args: serde_json::Value,
    ) -> ToolIntent {
        let id = opaque_id().unwrap();
        let request = opaque_id().unwrap();
        let message = opaque_id().unwrap();
        assert!(
            c.begin_request(
                turn,
                RequestIntent {
                    context: Default::default(),
                    id: request.clone(),
                    message_id: message.clone(),
                    ordinal,
                    requested_model: "opencode-go/glm-5.3-flash".into(),
                    requested_settings: Default::default(),
                    settings: Default::default()
                }
            )
            .await
            .unwrap()
        );
        let intent = ToolIntent {
            id: id.clone(),
            request_id: request.clone(),
            result_message_id: opaque_id().unwrap(),
            call_id: format!("call-{id}"),
            provider_call_id: format!("provider-{id}"),
            name: name.into(),
            arguments: args,
        };
        c.complete_request(
            turn,
            RequestCompletion {
                compaction: None,
                request_id: request,
                response: InferenceResponse {
                    resolved_model: None,
                    usage: Usage::default(),
                    finish_reason: FinishReason::ToolCalls,
                    message: InferenceMessage {
                        role: MessageRole::Assistant,
                        blocks: vec![ContentBlock {
                            id: format!("block-{id}"),
                            content: BlockContent::ToolCall {
                                call_id: intent.call_id.clone(),
                                provider_call_id: intent.provider_call_id.clone(),
                                name: name.into(),
                                arguments: intent.arguments.clone(),
                            },
                        }],
                        continuation: None,
                    },
                },
                tools: vec![intent.clone()],
            },
        )
        .await
        .unwrap();
        assert!(c.start_tool(turn, &intent).await.unwrap());
        intent
    }

    #[tokio::test]
    async fn schema_six_upgrade_backfills_dispatch_counts_and_preserves_job_receipts() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let mut legacy_spec = spec("legacy-job");
        legacy_spec.budget = None;
        let request = CreateTaskRequest {
            command_id: "legacy-job".into(),
            spec: legacy_spec.clone(),
        };
        let receipt = c
            .create_task(request.clone(), Some(legacy_spec))
            .await
            .unwrap();
        c.claim_next_turn().await.unwrap().unwrap();
        let intent = record(
            &c,
            &receipt.turn_id,
            1,
            "read",
            serde_json::json!({"path":"file.txt"}),
        )
        .await;
        c.finish_tool(
            &receipt.turn_id,
            &intent,
            completed("historical read".into()),
        )
        .await
        .unwrap();
        c.finish_chat_turn(&receipt.turn_id, outcome(None))
            .await
            .unwrap();
        c.submit(|db| conversations::legacy_fixture(db, 6))
            .await
            .unwrap();
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let c = store.client();
        assert_eq!(c.create_task(request, None).await.unwrap(), receipt);
        let task = c.task(&receipt.task_id).await.unwrap();
        assert_eq!(
            task.budget_usage,
            BudgetUsage {
                model_requests: 1,
                tool_calls: 1
            }
        );
        assert!(task.child.is_none());
        assert!(task.spec.budget.is_none());
        assert_eq!(task.latest_run.as_ref().unwrap().turn.status, "completed");
        assert_eq!(
            c.tool_invocations(&receipt.turn_id, None, 10)
                .await
                .unwrap()
                .items[0]
                .status,
            "completed"
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn schema_thirteen_queued_delegating_parent_keeps_legacy_policy_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let parent = root(&c, "schema-thirteen-parent").await;
        store.shutdown().await.unwrap();
        let db = Connection::open(data.join(DATABASE_FILE)).unwrap();
        db.execute("UPDATE sessions SET spec=json_remove(spec,'$.orchestration.max_concurrent_children'),requested_spec=json_remove(requested_spec,'$.orchestration.max_concurrent_children') WHERE id=?1",[&parent.task_id]).unwrap();
        workspaces::legacy_fixture(&db).unwrap();
        db.execute_batch("DROP TABLE child_notifications; ALTER TABLE child_links DROP COLUMN spawned_at_ms; ALTER TABLE child_links DROP COLUMN detached; ALTER TABLE child_links DROP COLUMN notification_mode; ALTER TABLE child_waits DROP COLUMN result; ALTER TABLE child_waits DROP COLUMN deadline_ms; ALTER TABLE child_waits DROP COLUMN mode; PRAGMA user_version=13;").unwrap();
        drop(db);
        let migrated = open(data).await;
        let claimed = migrated.client().claim_next_turn().await.unwrap().unwrap();
        assert_eq!(claimed.turn_id, parent.turn_id);
        migrated.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn schema_thirteen_receipts_replay_typed_subagent_requests_and_reject_changes() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let mut parent_spec = spec("legacy-replay-parent");
        parent_spec.orchestration = Some(policy());
        let parent_request = CreateTaskRequest {
            command_id: "legacy-replay-parent".into(),
            spec: parent_spec.clone(),
        };
        let parent = c
            .create_task(parent_request.clone(), Some(parent_spec))
            .await
            .unwrap();
        c.claim_next_turn().await.unwrap().unwrap();
        let child_request = child("legacy-replay-child");
        let child_receipt = c
            .create_child(
                &parent.run_id,
                child_request.clone(),
                Some(child_request.spec.clone()),
            )
            .await
            .unwrap();
        let wait_request = WaitChildrenRequest {
            command_id: "legacy-replay-wait".into(),
            child_run_ids: vec![child_receipt.run_id.clone()],
            mode: "all".into(),
            timeout_ms: None,
        };
        let wait_receipt = c
            .wait_children(&parent.run_id, wait_request.clone())
            .await
            .unwrap();
        c.finish_chat_turn(&parent.turn_id, outcome(Some("awaiting_children")))
            .await
            .unwrap();

        let session_request = CreateSessionRequest {
            command_id: "legacy-replay-session".into(),
            title: None,
            provider: "opencode-go".into(),
            model: "glm-5.3-flash".into(),
            max_tokens: Some(64),
            settings: None,
            workspace: Default::default(),
            prompt: Some("delegating session".into()),
            draft_id: None,
            budget: Some(OperationBudget {
                max_model_requests: 4,
                max_tool_calls: 4,
            }),
            orchestration: Some(policy()),
        };
        let session_receipt = c.create_session(session_request.clone()).await.unwrap();
        let batch_spec = BatchSpec {
            name: "legacy delegating batch".into(),
            prompts: vec!["batch child parent".into()],
            models: vec!["opencode-go/glm-5.3-flash".into()],
            settings: vec![Default::default()],
            workspace: Default::default(),
            max_concurrent_runs: 1,
            default_max_output_tokens: Some(64),
            budget: Some(OperationBudget {
                max_model_requests: 8,
                max_tool_calls: 8,
            }),
            orchestration: Some(policy()),
        };
        let batch_request = CreateBatchRequest {
            command_id: "legacy-replay-batch".into(),
            spec: batch_spec.clone(),
        };
        let preview = BatchPreviewResponse {
            combinations: orchestration::combinations(&batch_spec).unwrap(),
            spec: batch_spec.clone(),
        };
        let batch_receipt = c
            .create_batch(batch_request.clone(), Some(preview))
            .await
            .unwrap();
        // Model the exact schema-13 request bytes that those commands stored.
        let db = Connection::open(data.join(DATABASE_FILE)).unwrap();
        workspaces::legacy_fixture(&db).unwrap();
        db.execute_batch("UPDATE commands SET payload=json_remove(payload,'$.spec.orchestration.max_concurrent_children') WHERE scope='create_task'; UPDATE commands SET payload=json_remove(payload,'$.orchestration.max_concurrent_children') WHERE scope='create_session'; UPDATE commands SET payload=json_remove(payload,'$.spec.orchestration.max_concurrent_children') WHERE scope='create_batch'; UPDATE commands SET payload=json_remove(payload,'$.notification_mode','$.detached') WHERE scope LIKE 'run:%:child'; UPDATE commands SET payload=json_remove(payload,'$.mode','$.timeout_ms') WHERE scope LIKE 'run:%:wait'; DROP TABLE child_notifications; ALTER TABLE child_links DROP COLUMN spawned_at_ms; ALTER TABLE child_links DROP COLUMN detached; ALTER TABLE child_links DROP COLUMN notification_mode; ALTER TABLE child_waits DROP COLUMN result; ALTER TABLE child_waits DROP COLUMN deadline_ms; ALTER TABLE child_waits DROP COLUMN mode; PRAGMA user_version=13;").unwrap();
        db.execute(
            "UPDATE turns SET status='completed' WHERE id=?1",
            [&parent.turn_id],
        )
        .unwrap();
        drop(db);
        store.shutdown().await.unwrap();
        let migrated = open(data).await;
        let c = migrated.client();
        let mut legacy_task_json = serde_json::to_value(&parent_request).unwrap();
        legacy_task_json["spec"]["orchestration"]
            .as_object_mut()
            .unwrap()
            .remove("max_concurrent_children");
        let task_old: CreateTaskRequest = serde_json::from_value(legacy_task_json).unwrap();
        assert_eq!(c.create_task(task_old.clone(), None).await.unwrap(), parent);
        let mut legacy_child_json = serde_json::to_value(&child_request).unwrap();
        legacy_child_json
            .as_object_mut()
            .unwrap()
            .remove("notification_mode");
        legacy_child_json
            .as_object_mut()
            .unwrap()
            .remove("detached");
        let child_old: CreateChildRequest = serde_json::from_value(legacy_child_json).unwrap();
        assert_eq!(
            c.create_child(&parent.run_id, child_old.clone(), None)
                .await
                .unwrap(),
            child_receipt
        );
        let mut legacy_wait_json = serde_json::to_value(&wait_request).unwrap();
        legacy_wait_json.as_object_mut().unwrap().remove("mode");
        legacy_wait_json
            .as_object_mut()
            .unwrap()
            .remove("timeout_ms");
        let wait_old: WaitChildrenRequest = serde_json::from_value(legacy_wait_json).unwrap();
        assert_eq!(
            c.wait_children(&parent.run_id, wait_old.clone())
                .await
                .unwrap(),
            wait_receipt
        );
        let mut legacy_session_json = serde_json::to_value(&session_request).unwrap();
        legacy_session_json["orchestration"]
            .as_object_mut()
            .unwrap()
            .remove("max_concurrent_children");
        let session_old: CreateSessionRequest =
            serde_json::from_value(legacy_session_json).unwrap();
        assert_eq!(
            c.create_session(session_old.clone()).await.unwrap(),
            session_receipt
        );
        let mut legacy_batch_json = serde_json::to_value(&batch_request).unwrap();
        legacy_batch_json["spec"]["orchestration"]
            .as_object_mut()
            .unwrap()
            .remove("max_concurrent_children");
        let batch_old: CreateBatchRequest = serde_json::from_value(legacy_batch_json).unwrap();
        assert_eq!(
            c.create_batch(batch_old.clone(), None).await.unwrap(),
            batch_receipt
        );
        let mut changed = task_old;
        changed
            .spec
            .orchestration
            .as_mut()
            .unwrap()
            .max_concurrent_children = 2;
        assert!(matches!(
            c.create_task(changed, None).await,
            Err(StoreError::Conflict)
        ));
        let mut changed = child_old;
        changed.notification_mode = "manual".into();
        assert!(matches!(
            c.create_child(&parent.run_id, changed, None).await,
            Err(StoreError::Conflict)
        ));
        let mut changed = wait_old;
        changed.timeout_ms = Some(1000);
        assert!(matches!(
            c.wait_children(&parent.run_id, changed).await,
            Err(StoreError::Conflict)
        ));
        let mut changed = session_old;
        changed
            .orchestration
            .as_mut()
            .unwrap()
            .max_concurrent_children = 2;
        assert!(matches!(
            c.create_session(changed).await,
            Err(StoreError::Conflict)
        ));
        let mut changed = batch_old;
        changed
            .spec
            .orchestration
            .as_mut()
            .unwrap()
            .max_concurrent_children = 2;
        assert!(matches!(
            c.create_batch(changed, None).await,
            Err(StoreError::Conflict)
        ));
        let manual: bool = c
            .submit({
                let task = child_receipt.task_id.clone();
                move |db| {
                    db.query_row(
                        "SELECT notification_mode='manual' FROM child_links WHERE child_task_id=?1",
                        [task],
                        |r| r.get(0),
                    )
                    .map_err(|_| StoreError::Database)
                }
            })
            .await
            .unwrap();
        assert!(
            manual,
            "old child links retain conservative manual delivery despite receipt normalization"
        );
        migrated.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_limits_context_and_receipts_survive_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let parent = root(&c, "parent").await;
        let original = c.run(&parent.run_id).await.unwrap();
        let mut request = child("selected-child");
        request
            .context
            .message_ids
            .push(original.turn.user_message_id);
        let accepted = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec.clone()))
            .await
            .unwrap();
        let snapshot = c.task(&accepted.task_id).await.unwrap();
        assert_eq!(
            snapshot.child.as_ref().unwrap().parent_run_id,
            parent.run_id
        );
        assert!(snapshot.spec.prompt.contains("Selected public message"));
        assert_eq!(snapshot.requested_spec.prompt, "selected-child");
        let mut bad = child("unauthorized-model");
        bad.spec.model = "opencode-go/glm-5.3".into();
        assert!(matches!(
            c.create_child(&parent.run_id, bad.clone(), Some(bad.spec))
                .await,
            Err(StoreError::Unsupported(_))
        ));
        for id in ["child2", "child3", "child4"] {
            let r = child(id);
            c.create_child(&parent.run_id, r.clone(), Some(r.spec))
                .await
                .unwrap();
        }
        let r = child("overflow");
        assert!(matches!(
            c.create_child(&parent.run_id, r.clone(), Some(r.spec))
                .await,
            Err(StoreError::Limit)
        ));
        assert_eq!(
            c.children(&parent.run_id, None, 50)
                .await
                .unwrap()
                .items
                .len(),
            4
        );
        // The policy cap applies to work that is still outstanding. Completed
        // children remain in history but release one admission position.
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        c.finish_chat_turn(&parent.turn_id, outcome(None))
            .await
            .unwrap();
        let completed_child = c.claim_next_turn().await.unwrap().unwrap();
        c.finish_chat_turn(&completed_child.turn_id, outcome(None))
            .await
            .unwrap();
        let next = child("after-completion");
        c.create_child(&parent.run_id, next.clone(), Some(next.spec))
            .await
            .unwrap();
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let c = store.client();
        assert_eq!(
            c.create_child(&parent.run_id, request, None).await.unwrap(),
            accepted
        );
        assert_eq!(
            c.children(&parent.run_id, None, 50)
                .await
                .unwrap()
                .items
                .len(),
            5
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn full_pool_of_waiting_parents_releases_admission_for_children() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut parents = Vec::new();
        for i in 0..4 {
            parents.push(root(&c, &format!("parent-{i}")).await);
        }
        let mut children = Vec::new();
        for parent in &parents {
            assert_eq!(
                c.claim_next_turn().await.unwrap().unwrap().turn_id,
                parent.turn_id
            );
            let request = child(&format!("child-{}", parent.run_id));
            let accepted = c
                .create_child(&parent.run_id, request.clone(), Some(request.spec))
                .await
                .unwrap();
            c.wait_children(
                &parent.run_id,
                WaitChildrenRequest {
                    command_id: format!("wait-{}", parent.run_id),
                    child_run_ids: vec![accepted.run_id.clone()],
                    mode: "all".into(),
                    timeout_ms: None,
                },
            )
            .await
            .unwrap();
            c.finish_chat_turn(&parent.turn_id, outcome(Some("awaiting_children")))
                .await
                .unwrap();
            assert_eq!(
                c.run(&parent.run_id).await.unwrap().turn.status,
                "awaiting_children"
            );
            children.push(accepted);
        }
        let mut active = Vec::new();
        for _ in 0..4 {
            active.push(c.claim_next_turn().await.unwrap().unwrap());
        }
        assert!(
            active
                .iter()
                .all(|w| children.iter().any(|child| child.turn_id == w.turn_id))
        );
        for child in active {
            c.finish_chat_turn(&child.turn_id, outcome(None))
                .await
                .unwrap();
        }
        for parent in &parents {
            assert_eq!(c.run(&parent.run_id).await.unwrap().turn.status, "queued");
        }
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn any_join_completes_independently_of_attached_siblings_and_notices_are_durable() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "any-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let a = child("any-a");
        let a = c
            .create_child(&parent.run_id, a.clone(), Some(a.spec))
            .await
            .unwrap();
        let b = child("any-b");
        let b = c
            .create_child(&parent.run_id, b.clone(), Some(b.spec))
            .await
            .unwrap();
        let wait = record(
            &c,
            &parent.turn_id,
            1,
            "child_wait",
            serde_json::json!({"child_run_ids":[a.run_id,b.run_id],"mode":"any"}),
        )
        .await;
        assert!(matches!(
            c.native_orchestration(&parent.turn_id, &wait)
                .await
                .unwrap(),
            OrchestrationResult::Waiting
        ));
        c.finish_chat_turn(&parent.turn_id, outcome(Some("awaiting_children")))
            .await
            .unwrap();
        let child_work = c.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(child_work.turn_id, a.turn_id);
        c.finish_chat_turn(&a.turn_id, outcome(None)).await.unwrap();
        let OrchestrationResult::Completed(result) = c
            .native_orchestration(&parent.turn_id, &wait)
            .await
            .unwrap()
        else {
            panic!("any join should finish after first terminal child")
        };
        assert!(result.output.contains(&a.run_id));
        assert!(result.output.contains(&b.run_id));
        let original_wait_result = result.output.clone();
        assert_eq!(c.run(&b.run_id).await.unwrap().turn.status, "queued");
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            b.turn_id
        );
        c.finish_chat_turn(&b.turn_id, outcome(None)).await.unwrap();
        let OrchestrationResult::Completed(replayed) = c
            .native_orchestration(&parent.turn_id, &wait)
            .await
            .unwrap()
        else {
            panic!("accepted any join must retain its terminal result")
        };
        assert_eq!(replayed.output, original_wait_result);
        let pending_notices:u32=c.submit({let session=parent.session_id.clone();move |db|db.query_row("SELECT count(*) FROM child_notifications WHERE delivered=0 AND parent_session_id=?1",[session],|r|r.get(0)).map_err(|_|StoreError::Database)}).await.unwrap();
        assert_eq!(
            pending_notices, 2,
            "each completion notice is durable before the parent reads them"
        );
        let replan = c
            .begin_request(
                &parent.turn_id,
                RequestIntent {
                    id: "notice-boundary-request".into(),
                    message_id: "notice-boundary-message".into(),
                    ordinal: 2,
                    requested_model: "opencode-go/glm-5.3-flash".into(),
                    requested_settings: Default::default(),
                    settings: Default::default(),
                    context: Default::default(),
                },
            )
            .await
            .unwrap();
        assert!(
            !replan,
            "notice delivery yields so the next provider request reloads durable history"
        );
        let visible:bool=c.submit({let session=parent.session_id.clone();move |db|db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND text LIKE '%task_id%')",[session],|r|r.get(0)).map_err(|_|StoreError::Database)}).await.unwrap();
        assert!(visible);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn implicit_join_snapshots_earliest_unfinished_child_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "implicit-wait-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let request = child("implicit-wait-child");
        let child = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child.turn_id
        );
        c.send_message(
            &child.session_id,
            SendMessageRequest {
                command_id: "implicit-wait-followup".into(),
                text: "continue after this turn".into(),
                delivery: Some(DeliveryMode::AfterFinish),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let followup: String = c
            .submit({
                let session = child.session_id.clone();
                move |db| {
                    db.query_row(
                        "SELECT turn_id FROM message_queue WHERE session_id=?1 AND status='pending' ORDER BY sequence DESC LIMIT 1",
                        [session],
                        |r| r.get(0),
                    )
                    .map_err(|_| StoreError::Database)
                }
            })
            .await
            .unwrap();

        let wait = record(
            &c,
            &parent.turn_id,
            1,
            "child_wait",
            serde_json::json!({"mode":"all"}),
        )
        .await;
        assert!(matches!(
            c.native_orchestration(&parent.turn_id, &wait)
                .await
                .unwrap(),
            OrchestrationResult::Waiting
        ));
        c.cancel_turn(
            &followup,
            CancelTurnRequest {
                command_id: "cancel-implicit-followup".into(),
            },
        )
        .await
        .unwrap();
        let selected: Vec<String> = c
            .submit({
                let wait_id = format!("wait-{}", wait.id);
                move |db| {
                    db.query_row(
                        "SELECT child_run_ids FROM child_waits WHERE id=?1",
                        [wait_id],
                        |r| execution::json_column(r, 0),
                    )
                    .map_err(|_| StoreError::Database)
                }
            })
            .await
            .unwrap();
        assert_eq!(selected, vec![child.run_id.clone()]);
        assert!(
            matches!(
                c.native_orchestration(&parent.turn_id, &wait)
                    .await
                    .unwrap(),
                OrchestrationResult::Waiting
            ),
            "a canceled later follow-up must not satisfy the join"
        );

        c.finish_chat_turn(&parent.turn_id, outcome(Some("awaiting_children")))
            .await
            .unwrap();
        c.finish_chat_turn(&child.turn_id, outcome(None))
            .await
            .unwrap();
        let OrchestrationResult::Completed(result) = c
            .native_orchestration(&parent.turn_id, &wait)
            .await
            .unwrap()
        else {
            panic!("implicit wait should complete when the original attempt ends")
        };
        assert!(result.output.contains(&child.run_id));
        assert!(result.output.contains("\"pending_run_ids\":[]"));
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_followup_reactivation_consumes_outstanding_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut parent_spec = spec("reactivation-parent");
        let mut authority = policy();
        authority.max_children = 1;
        authority.max_concurrent_children = 1;
        parent_spec.orchestration = Some(authority);
        let parent = c
            .create_task(
                CreateTaskRequest {
                    command_id: "reactivation-parent".into(),
                    spec: parent_spec.clone(),
                },
                Some(parent_spec),
            )
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        let first = child("reactivated-child");
        let child_receipt = c
            .create_child(&parent.run_id, first.clone(), Some(first.spec))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child_receipt.turn_id
        );
        c.finish_chat_turn(&child_receipt.turn_id, outcome(None))
            .await
            .unwrap();
        c.send_message(
            &child_receipt.session_id,
            SendMessageRequest {
                command_id: "reactivate-child".into(),
                text: "continue".into(),
                delivery: Some(DeliveryMode::AfterFinish),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let next = child("next-child");
        assert!(matches!(
            c.create_child(&parent.run_id, next.clone(), Some(next.spec.clone()))
                .await,
            Err(StoreError::Limit)
        ));
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_send_is_owned_and_replays_receipt_after_child_state_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "steer-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let request = child("steer-child");
        let child = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let intent=record(&c,&parent.turn_id,1,"child_send",serde_json::json!({"task_id":child.task_id,"text":"focus on the failing test","delivery":"after-turn"})).await;
        let OrchestrationResult::Completed(first) = c
            .native_orchestration(&parent.turn_id, &intent)
            .await
            .unwrap()
        else {
            panic!("steering should be accepted")
        };
        c.cancel_turn(
            &child.turn_id,
            CancelTurnRequest {
                command_id: "cancel-after-steer".into(),
            },
        )
        .await
        .unwrap();
        let OrchestrationResult::Completed(replayed) = c
            .native_orchestration(&parent.turn_id, &intent)
            .await
            .unwrap()
        else {
            panic!("receipt replay must survive child cancellation")
        };
        assert_eq!(first.output, replayed.output);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_progress_and_cancel_use_unfinished_turns_not_latest_followup() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "active-followups-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let request = child("active-followups-child");
        let child = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child.turn_id
        );

        for (command_id, text) in [("queue-followup-one", "one"), ("queue-followup-two", "two")] {
            c.send_message(
                &child.session_id,
                SendMessageRequest {
                    command_id: command_id.into(),
                    text: text.into(),
                    delivery: Some(DeliveryMode::AfterFinish),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let queued: Vec<String> = c
            .submit({
                let session = child.session_id.clone();
                move |db| {
                    let mut stmt = db.prepare("SELECT turn_id FROM message_queue WHERE session_id=?1 AND status='pending' ORDER BY sequence").map_err(|_|StoreError::Database)?;
                    stmt.query_map([session],|r|r.get(0)).map_err(|_|StoreError::Database)?.collect::<Result<Vec<_>,_>>().map_err(|_|StoreError::Database)
                }
            })
            .await
            .unwrap();
        assert_eq!(queued.len(), 2);
        c.submit({
            let later = queued[1].clone();
            move |db| {
                db.execute(
                    "UPDATE message_queue SET request=json_set(request,'$.model','openai/gpt-4.1-mini') WHERE turn_id=?1",
                    [later],
                )
                .map_err(|_| StoreError::Database)?;
                Ok(())
            }
        })
        .await
        .unwrap();
        let request_id = opaque_id().unwrap();
        let make_intent = |name: &str| {
            let id = opaque_id().unwrap();
            ToolIntent {
                id: id.clone(),
                request_id: request_id.clone(),
                result_message_id: opaque_id().unwrap(),
                call_id: format!("call-{id}"),
                provider_call_id: format!("provider-{id}"),
                name: name.into(),
                arguments: serde_json::json!({"task_id":child.task_id}),
            }
        };
        let inspect = make_intent("child_inspect");
        let cancel = make_intent("child_cancel");
        let request_message = opaque_id().unwrap();
        assert!(
            c.begin_request(
                &parent.turn_id,
                RequestIntent {
                    context: Default::default(),
                    id: request_id.clone(),
                    message_id: request_message.clone(),
                    ordinal: 1,
                    requested_model: "opencode-go/glm-5.3-flash".into(),
                    requested_settings: Default::default(),
                    settings: Default::default(),
                }
            )
            .await
            .unwrap()
        );
        c.complete_request(
            &parent.turn_id,
            RequestCompletion {
                compaction: None,
                request_id,
                response: InferenceResponse {
                    resolved_model: None,
                    usage: Usage::default(),
                    finish_reason: FinishReason::ToolCalls,
                    message: InferenceMessage {
                        role: MessageRole::Assistant,
                        blocks: vec![inspect.clone(), cancel.clone()]
                            .into_iter()
                            .map(|tool| ContentBlock {
                                id: format!("block-{}", tool.id),
                                content: BlockContent::ToolCall {
                                    call_id: tool.call_id.clone(),
                                    provider_call_id: tool.provider_call_id.clone(),
                                    name: tool.name.clone(),
                                    arguments: tool.arguments.clone(),
                                },
                            })
                            .collect(),
                        continuation: None,
                    },
                },
                tools: vec![inspect.clone(), cancel.clone()],
            },
        )
        .await
        .unwrap();
        assert!(c.start_tool(&parent.turn_id, &inspect).await.unwrap());
        assert!(c.start_tool(&parent.turn_id, &cancel).await.unwrap());
        c.cancel_turn(
            &queued[0],
            CancelTurnRequest {
                command_id: "cancel-one-followup".into(),
            },
        )
        .await
        .unwrap();
        let OrchestrationResult::Completed(progress) = c
            .native_orchestration(&parent.turn_id, &inspect)
            .await
            .unwrap()
        else {
            panic!("inspection should complete")
        };
        let progress: serde_json::Value = serde_json::from_str(&progress.output).unwrap();
        assert_eq!(progress["run_id"], child.run_id);
        assert_eq!(progress["latest_run_id"], child.run_id);
        assert_eq!(progress["status"], "running");
        assert_eq!(progress["model"], "opencode-go/glm-5.3-flash");

        let OrchestrationResult::Completed(first) = c
            .native_orchestration(&parent.turn_id, &cancel)
            .await
            .unwrap()
        else {
            panic!("cancellation should complete")
        };
        let report: serde_json::Value = serde_json::from_str(&first.output).unwrap();
        assert_eq!(report["cancelled_count"], 2);
        assert_eq!(report["cancelled_run_ids"].as_array().unwrap().len(), 2);
        assert_eq!(c.run(&queued[1]).await.unwrap().turn.status, "cancelled");
        assert!(
            c.submit({
                let running = child.turn_id.clone();
                move |db| cancellation_requested(db, &running)
            })
            .await
            .unwrap()
        );
        let OrchestrationResult::Completed(replayed) = c
            .native_orchestration(&parent.turn_id, &cancel)
            .await
            .unwrap()
        else {
            panic!("aggregate cancellation outcome must replay")
        };
        assert_eq!(first, replayed);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_inspection_reports_spawn_age_and_gap_free_bounded_event_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "inspect-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let req = child("inspect-child");
        let child = c
            .create_child(&parent.run_id, req.clone(), Some(req.spec))
            .await
            .unwrap();
        c.submit({
            let session = child.session_id.clone();
            move |db| {
                let tx = db
                    .unchecked_transaction()
                    .map_err(|_| StoreError::Database)?;
                append_event(&tx, &session, "inspect_test_a", None, None)?;
                append_event(&tx, &session, "inspect_test_b", None, None)?;
                tx.commit().map_err(|_| StoreError::Database)
            }
        })
        .await
        .unwrap();
        let inspect = record(
            &c,
            &parent.turn_id,
            1,
            "child_inspect",
            serde_json::json!({"task_id":child.task_id,"event_after":0,"event_limit":1}),
        )
        .await;
        let OrchestrationResult::Completed(first) = c
            .native_orchestration(&parent.turn_id, &inspect)
            .await
            .unwrap()
        else {
            panic!("inspection should complete")
        };
        let first: serde_json::Value = serde_json::from_str(&first.output).unwrap();
        assert!(first["spawned_at_ms"].as_i64().is_some());
        assert!(first["elapsed_since_spawn_ms"].as_i64().is_some());
        assert_eq!(first["events"].as_array().unwrap().len(), 1);
        let cursor = first["next_event_cursor"].as_u64().unwrap();
        let inspect = record(
            &c,
            &parent.turn_id,
            2,
            "child_inspect",
            serde_json::json!({"task_id":child.task_id,"event_after":cursor,"event_limit":1}),
        )
        .await;
        let OrchestrationResult::Completed(second) = c
            .native_orchestration(&parent.turn_id, &inspect)
            .await
            .unwrap()
        else {
            panic!("next event page should complete")
        };
        let second: serde_json::Value = serde_json::from_str(&second.output).unwrap();
        assert_eq!(
            second["events"][0]["sequence"].as_u64().unwrap(),
            cursor + 1
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn completion_notice_batch_shrinks_control_heavy_previews_without_losing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut spec = spec("notice-batch-parent");
        let mut p = policy();
        p.max_children = 8;
        spec.orchestration = Some(p);
        let parent = c
            .create_task(
                CreateTaskRequest {
                    command_id: "notice-batch-parent".into(),
                    spec: spec.clone(),
                },
                Some(spec),
            )
            .await
            .unwrap();
        c.claim_next_turn().await.unwrap().unwrap();
        let mut turns = Vec::new();
        for n in 0..8 {
            let req = child(&format!("notice-batch-child-{n}"));
            turns.push(
                c.create_child(&parent.run_id, req.clone(), Some(req.spec))
                    .await
                    .unwrap()
                    .turn_id,
            );
        }
        let parent_turn = parent.turn_id.clone();
        let text=c.submit(move |db|{let tx=db.unchecked_transaction().map_err(|_|StoreError::Database)?;for turn in &turns {let user:String=tx.query_row("SELECT user_message_id FROM turns WHERE id=?1",[turn],|r|r.get(0)).map_err(|_|StoreError::Database)?;tx.execute("UPDATE turns SET status='completed',assistant_message_id=?2 WHERE id=?1",rusqlite::params![turn,user]).map_err(|_|StoreError::Database)?;tx.execute("UPDATE messages SET text=?2 WHERE id=?1",rusqlite::params![user,"\u{0001}\n".repeat(2048)]).map_err(|_|StoreError::Database)?;children::enqueue_completion(&tx,turn)?;}children::wake(&tx)?;assert!(deliver_notifications(&tx,&parent_turn)?);let text:String=tx.query_row("SELECT text FROM messages WHERE turn_id=?1 AND role='user' ORDER BY ordinal DESC LIMIT 1",[&parent_turn],|r|r.get(0)).map_err(|_|StoreError::Database)?;assert!(text.len()<=hive_runtime::tools::PREVIEW_BYTES);let entries:Vec<serde_json::Value>=serde_json::from_str(text.split_once(": ").ok_or(StoreError::Database)?.1).map_err(|_|StoreError::Database)?;assert_eq!(entries.len(),8);assert!(entries.iter().all(|entry|entry["output_truncated"]==true));for turn in turns {assert!(entries.iter().any(|entry|entry["run_id"]==turn));}tx.commit().map_err(|_|StoreError::Database)?;Ok(text)}).await.unwrap();
        assert!(text.contains("Child task completion notices"));
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_discovery_pages_supported_models_without_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut spec = spec("discovery-parent");
        let mut authority = policy();
        authority.allowed_models.clear();
        spec.orchestration = Some(authority);
        let parent = c
            .create_task(
                CreateTaskRequest {
                    command_id: "discovery-parent".into(),
                    spec: spec.clone(),
                },
                Some(spec),
            )
            .await
            .unwrap();
        c.claim_next_turn().await.unwrap().unwrap();
        let mut actual = Vec::new();
        let mut after: Option<String> = None;
        let mut ordinal = 1;
        loop {
            let mut args = serde_json::json!({"limit":32});
            if let Some(cursor) = &after {
                args["after"] = serde_json::Value::String(cursor.clone());
            }
            let intent = record(&c, &parent.turn_id, ordinal, "child_discover", args).await;
            ordinal += 1;
            let OrchestrationResult::Completed(outcome) = c
                .native_orchestration(&parent.turn_id, &intent)
                .await
                .unwrap()
            else {
                panic!("discovery should complete")
            };
            let page: serde_json::Value = serde_json::from_str(&outcome.output).unwrap();
            let ids: Vec<String> = page["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|model| model["id"].as_str().unwrap().to_owned())
                .collect();
            actual.extend(ids);
            after = page["next_after"].as_str().map(str::to_owned);
            if after.is_none() {
                break;
            }
        }
        let mut expected = Vec::new();
        for provider_id in hive_core::provider::ProviderId::all() {
            if let Some(provider) = hive_runtime::providers::provider(*provider_id) {
                for model in hive_runtime::providers::catalog::models(provider) {
                    expected.push(format!("{}/{}", provider_id.as_str(), model.id));
                }
            }
        }
        expected.sort();
        assert_eq!(actual, expected);
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_wait_recovery_preserves_known_tool_results_and_requires_resume() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let store = open(data.clone()).await;
        let c = store.client();
        let parent = root(&c, "native-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let request = child("native-child");
        let child = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let intent = record(
            &c,
            &parent.turn_id,
            1,
            "child_wait",
            serde_json::json!({"child_run_ids":[child.run_id]}),
        )
        .await;
        assert!(matches!(
            c.native_orchestration(&parent.turn_id, &intent)
                .await
                .unwrap(),
            OrchestrationResult::Waiting
        ));
        // Crash between committing the wait and releasing the runtime slot.
        store.shutdown().await.unwrap();
        let store = open(data).await;
        let c = store.client();
        assert_eq!(c.run(&parent.run_id).await.unwrap().turn.status, "paused");
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child.turn_id
        );
        c.finish_chat_turn(&child.turn_id, outcome(None))
            .await
            .unwrap();
        assert!(c.claim_next_turn().await.unwrap().is_none());
        let invocation = c
            .tool_invocations(&parent.turn_id, None, 10)
            .await
            .unwrap()
            .items
            .pop()
            .unwrap();
        assert_eq!(invocation.status, "completed");
        assert!(!c.run(&parent.run_id).await.unwrap().effects_unknown);
        c.resume_turn(
            &parent.turn_id,
            TurnControlRequest {
                command_id: "resume-wait".into(),
            },
        )
        .await
        .unwrap();
        let work = c.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, parent.turn_id);
        assert_eq!(work.next_request_ordinal, 2);
        assert!(work.history.iter().any(|m| m.role == MessageRole::Tool));
        assert_eq!(
            c.children(&parent.run_id, None, 10)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pause_during_native_wait_preserves_dependency_and_known_result() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "pause-wait-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let request = child("pause-wait-child");
        let child = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let intent = record(
            &c,
            &parent.turn_id,
            1,
            "child_wait",
            serde_json::json!({"child_run_ids":[child.run_id]}),
        )
        .await;
        // Retrying the accepted internal operation reuses its dependency.
        for _ in 0..2 {
            assert!(matches!(
                c.native_orchestration(&parent.turn_id, &intent)
                    .await
                    .unwrap(),
                OrchestrationResult::Waiting
            ));
        }
        c.pause_turn(
            &parent.turn_id,
            TurnControlRequest {
                command_id: "pause-wait".into(),
            },
        )
        .await
        .unwrap();
        let mut yield_outcome = outcome(Some("awaiting_children"));
        yield_outcome.status = ChatStatus::Interrupted;
        c.finish_chat_turn(&parent.turn_id, yield_outcome)
            .await
            .unwrap();
        assert_eq!(c.run(&parent.run_id).await.unwrap().turn.status, "paused");
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child.turn_id
        );
        c.finish_chat_turn(&child.turn_id, outcome(None))
            .await
            .unwrap();
        assert_eq!(c.run(&parent.run_id).await.unwrap().turn.status, "paused");
        assert!(!c.run(&parent.run_id).await.unwrap().effects_unknown);
        let OrchestrationResult::Completed(result) = c
            .native_orchestration(&parent.turn_id, &intent)
            .await
            .unwrap()
        else {
            panic!("expected known dependency result");
        };
        assert!(result.output.contains(&child.run_id));
        let waits: u32 = c
            .submit(move |db| {
                db.query_row("SELECT count(*) FROM child_waits", [], |r| r.get(0))
                    .map_err(|_| StoreError::Database)
            })
            .await
            .unwrap();
        assert_eq!(waits, 1);
        c.resume_turn(
            &parent.turn_id,
            TurnControlRequest {
                command_id: "resume-paused-wait".into(),
            },
        )
        .await
        .unwrap();
        let work = c.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, parent.turn_id);
        assert_eq!(work.next_request_ordinal, 2);
        assert!(work.history.iter().any(|m| m.role == MessageRole::Tool));
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_creation_commits_child_and_tool_outcome_together() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "native-create-parent").await;
        c.claim_next_turn().await.unwrap().unwrap();
        let intent = record(
            &c,
            &parent.turn_id,
            1,
            "child_create",
            serde_json::json!({"spec":spec("native-created")}),
        )
        .await;
        c.submit(|db|db.execute_batch("CREATE TRIGGER fail_child BEFORE INSERT ON child_links BEGIN SELECT RAISE(ABORT,'injected'); END;").map_err(|_|StoreError::Database)).await.unwrap();
        assert!(matches!(
            c.native_orchestration(&parent.turn_id, &intent).await,
            Err(StoreError::Database)
        ));
        assert!(
            c.children(&parent.run_id, None, 10)
                .await
                .unwrap()
                .items
                .is_empty()
        );
        c.submit(|db| {
            db.execute_batch("DROP TRIGGER fail_child")
                .map_err(|_| StoreError::Database)
        })
        .await
        .unwrap();
        let OrchestrationResult::Completed(result) = c
            .native_orchestration(&parent.turn_id, &intent)
            .await
            .unwrap()
        else {
            panic!("expected creation result")
        };
        let receipt: TaskReceipt = serde_json::from_str(&result.output).unwrap();
        c.finish_tool(&parent.turn_id, &intent, result)
            .await
            .unwrap();
        assert_eq!(
            c.children(&parent.run_id, None, 10).await.unwrap().items[0].id,
            receipt.task_id
        );
        let OrchestrationResult::Completed(repeated) = c
            .native_orchestration(&parent.turn_id, &intent)
            .await
            .unwrap()
        else {
            panic!("expected receipt")
        };
        assert_eq!(
            serde_json::from_str::<TaskReceipt>(&repeated.output).unwrap(),
            receipt
        );
        assert_eq!(
            c.children(&parent.run_id, None, 10)
                .await
                .unwrap()
                .items
                .len(),
            1
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn batch_cap_and_cancellation_include_independent_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let task_spec = spec("batch-parent");
        let batch_spec = BatchSpec {
            name: "delegating batch".into(),
            prompts: vec![task_spec.prompt],
            models: vec![task_spec.model],
            settings: vec![task_spec.settings],
            workspace: Default::default(),
            max_concurrent_runs: 1,
            default_max_output_tokens: Some(64),
            budget: task_spec.budget,
            orchestration: Some(policy()),
        };
        let prepared = BatchPreviewResponse {
            combinations: orchestration::combinations(&batch_spec).unwrap(),
            spec: batch_spec.clone(),
        };
        let batch = c
            .create_batch(
                CreateBatchRequest {
                    command_id: "delegating-batch".into(),
                    spec: batch_spec,
                },
                Some(prepared),
            )
            .await
            .unwrap();
        let parent = &batch.members[0];
        let request = child("batch-child");
        let child_run = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let mut request = child("batch-independent");
        request.cancel_with_parent = false;
        let independent = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        assert!(
            c.claim_next_turn().await.unwrap().is_none(),
            "children must share the batch concurrency cap"
        );
        c.wait_children(
            &parent.run_id,
            WaitChildrenRequest {
                command_id: "batch-child-wait".into(),
                child_run_ids: vec![child_run.run_id.clone()],
                mode: "all".into(),
                timeout_ms: None,
            },
        )
        .await
        .unwrap();
        c.finish_chat_turn(&parent.turn_id, outcome(Some("awaiting_children")))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            child_run.turn_id
        );
        let cancel = c
            .cancel_batch(
                &batch.batch_id,
                TurnControlRequest {
                    command_id: "cancel-child-batch".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(cancel.run_ids.len(), 3);
        assert_eq!(
            c.run(&parent.run_id).await.unwrap().turn.status,
            "cancelled"
        );
        assert_eq!(
            c.run(&independent.run_id).await.unwrap().turn.status,
            "cancelled"
        );
        c.finish_chat_turn(&child_run.turn_id, outcome(None))
            .await
            .unwrap();
        assert_eq!(
            c.run(&child_run.run_id).await.unwrap().turn.status,
            "cancelled"
        );
        assert!(c.claim_next_turn().await.unwrap().is_none());
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_admission_preserves_parent_snapshot_and_strips_legacy_authority() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut parent_spec = spec("admission-parent");
        let mut limits = policy();
        limits.max_concurrent_children = 1;
        parent_spec.orchestration = Some(limits);
        let parent = c
            .create_task(
                CreateTaskRequest {
                    command_id: "admission-parent".into(),
                    spec: parent_spec.clone(),
                },
                Some(parent_spec),
            )
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        let first = child("admission-first");
        let first = c
            .create_child(&parent.run_id, first.clone(), Some(first.spec))
            .await
            .unwrap();
        let second = child("admission-second");
        let second = c
            .create_child(&parent.run_id, second.clone(), Some(second.spec))
            .await
            .unwrap();
        // Simulate an older child whose stored policy previously permitted nesting.
        c.submit({
            let id = first.task_id.clone();
            let legacy = canonical(&policy()).unwrap();
            move |db| {
                db.execute("UPDATE sessions SET spec=json_set(spec,'$.orchestration',json(?2)) WHERE id=?1", rusqlite::params![id, legacy])
                    .map(|_| ())
                    .map_err(|_| StoreError::Database)
            }
        }).await.unwrap();
        let work = c.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.turn_id, first.turn_id);
        assert!(
            work.orchestration.is_none(),
            "legacy children must receive no orchestration tools"
        );
        assert!(
            c.claim_next_turn().await.unwrap().is_none(),
            "the queued sibling must respect the child concurrency cap"
        );
        c.finish_chat_turn(&parent.turn_id, outcome(None))
            .await
            .unwrap();
        assert_eq!(
            c.run(&parent.run_id).await.unwrap().turn.status,
            "awaiting_children"
        );
        assert!(
            c.retain_provider_snapshot(&parent.turn_id).await.unwrap(),
            "automatic attachment waiting is a same-turn continuation"
        );
        c.finish_chat_turn(&first.turn_id, outcome(None))
            .await
            .unwrap();
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            parent.turn_id
        );
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            second.turn_id
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn unrestricted_children_select_other_providers_and_model_specific_effort() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let mut parent_spec = spec("cross-provider-parent");
        let mut authority = policy();
        authority.allowed_models.clear();
        parent_spec.orchestration = Some(authority);
        let parent = c
            .create_task(
                CreateTaskRequest {
                    command_id: "cross-provider-parent".into(),
                    spec: parent_spec.clone(),
                },
                Some(parent_spec),
            )
            .await
            .unwrap();
        for (id, model, effort) in [
            ("zen-child", "opencode-zen/glm-5.3", Some("max")),
            ("go-child", "opencode-go/glm-5.3-flash", Some("high")),
            ("codex-child", "codex/gpt-6-luna", Some("high")),
        ] {
            let mut request = child(id);
            request.spec.model = model.into();
            request.spec.settings.reasoning_effort = effort.map(str::to_owned);
            let receipt = c
                .create_child(&parent.run_id, request.clone(), Some(request.spec))
                .await
                .unwrap();
            let run = c.run(&receipt.run_id).await.unwrap();
            assert_eq!(run.turn.requested_model, model);
            assert_eq!(run.turn.settings.reasoning_effort.as_deref(), effort);
        }
        let mut unsupported = child("unsupported-child-effort");
        unsupported.spec.settings.reasoning_effort = Some("unsupported-effort".into());
        assert!(matches!(
            c.create_child(&parent.run_id, unsupported.clone(), Some(unsupported.spec))
                .await,
            Err(StoreError::Unsupported("settings.reasoning_effort"))
        ));
        assert_eq!(
            c.children(&parent.run_id, None, 50)
                .await
                .unwrap()
                .items
                .len(),
            3
        );
        store.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn child_leaf_authority_and_cancellation_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "tree-parent").await;
        let request = child("branch");
        let branch = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let request = child("leaf");
        assert!(
            c.create_child(&branch.run_id, request.clone(), Some(request.spec))
                .await
                .is_err()
        );
        let mut request = child("independent");
        request.cancel_with_parent = false;
        let independent = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        c.cancel_turn(
            &parent.turn_id,
            CancelTurnRequest {
                command_id: "cancel-tree".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            c.run(&branch.run_id).await.unwrap().turn.status,
            "cancelled"
        );
        assert_eq!(
            c.run(&independent.run_id).await.unwrap().turn.status,
            "queued"
        );
        assert_eq!(
            c.claim_next_turn().await.unwrap().unwrap().turn_id,
            independent.turn_id
        );
        c.begin_request(
            &independent.turn_id,
            RequestIntent {
                context: Default::default(),
                id: "counted-child".into(),
                message_id: "counted-message".into(),
                ordinal: 1,
                requested_model: "opencode-go/glm-5.3-flash".into(),
                settings: Default::default(),
                requested_settings: Default::default(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            c.task(&parent.task_id)
                .await
                .unwrap()
                .budget_usage
                .model_requests,
            1
        );
        assert_eq!(
            c.task(&independent.task_id)
                .await
                .unwrap()
                .budget_usage
                .model_requests,
            1
        );
        let denied = c.task(&branch.task_id).await.unwrap();
        assert!(matches!(
            c.retry_run(
                &branch.run_id,
                RetryRunRequest {
                    command_id: "retry-cancelled-child".into(),
                    acknowledge_unknown_effects: false
                },
                Some(denied.spec)
            )
            .await,
            Err(StoreError::Conflict)
        ));
        store.shutdown().await.unwrap();
    }
}
