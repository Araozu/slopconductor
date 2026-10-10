//! Child acceptance, selected context, dependency waits, and cancellation share
//! the same transactional application operations for API and native tools.
use super::*;
use slop_protocol::orchestration::*;
use slop_runtime::{agent::ToolIntent, orchestration::OrchestrationResult, tools::ToolOutcome};
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

pub(super) fn link(c: &Connection, task: &str) -> StoreResult<Option<ChildLink>> {
    c.query_row("SELECT parent_task_id,parent_run_id,root_task_id,depth,cancel_with_parent,context FROM child_links WHERE child_task_id=?1",[task],|r|Ok(ChildLink{parent_task_id:r.get(0)?,parent_run_id:r.get(1)?,root_task_id:r.get(2)?,depth:r.get(3)?,cancel_with_parent:r.get(4)?,context:execution::json_column(r,5)?})).optional().map_err(|_|StoreError::Database)
}

pub(crate) fn validate_policy(spec: &TaskSpec) -> StoreResult<()> {
    let Some(policy) = &spec.orchestration else {
        return Ok(());
    };
    let runtime: slop_runtime::orchestration::OrchestrationPolicy =
        serde_json::from_str(&canonical(policy)?).map_err(|_| StoreError::Invalid)?;
    if !slop_runtime::orchestration::validate(&runtime)
        || spec.budget.is_none()
        || spec.model.starts_with("codex/")
        || policy.allowed_tools.iter().any(|tool| {
            spec.project
                .as_ref()
                .is_none_or(|p| !p.allowed_tools.contains(tool))
        })
    {
        return Err(StoreError::Invalid);
    }
    for model in &policy.allowed_models {
        execution::validate_settings(model, &Default::default())?;
    }
    Ok(())
}

fn parent(c: &Connection, id: &str) -> StoreResult<TaskResponse> {
    let run = orchestration::run(c, id)?;
    let task = orchestration::task(c, &run.task_id)?;
    if task.latest_run.id != id
        || !matches!(
            run.turn.status.as_str(),
            "queued" | "running" | "paused" | "awaiting_children"
        )
        || task.spec.orchestration.is_none()
    {
        return Err(StoreError::Conflict);
    }
    if let Some(batch) = c
        .query_row(
            "SELECT admission_batch_id FROM tasks WHERE id=?1",
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
    Ok(task)
}

fn constrain(
    c: &Connection,
    parent: &TaskResponse,
    spec: &mut TaskSpec,
) -> StoreResult<(String, u32)> {
    let policy = parent
        .spec
        .orchestration
        .as_ref()
        .ok_or(StoreError::Conflict)?;
    if !policy.allowed_models.contains(&spec.model) {
        return Err(StoreError::Unsupported("child.model"));
    }
    match (&parent.spec.project, &mut spec.project) {
        (Some(p), Some(child)) if p.project_id == child.project_id => {
            if child
                .allowed_tools
                .iter()
                .any(|t| !policy.allowed_tools.contains(t))
                || child
                    .base_ref
                    .as_ref()
                    .is_some_and(|b| Some(b) != p.base_ref.as_ref())
            {
                return Err(StoreError::Unsupported("child.project"));
            }
            child.base_ref = p.base_ref.clone();
        }
        (_, None) => {}
        _ => return Err(StoreError::Unsupported("child.project")),
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
    if let Some(child) = &spec.orchestration
        && (policy.max_depth <= 1
            || child.max_depth >= policy.max_depth
            || child.max_children > policy.max_children
            || child
                .allowed_models
                .iter()
                .any(|m| !policy.allowed_models.contains(m))
            || child
                .allowed_tools
                .iter()
                .any(|t| !policy.allowed_tools.contains(t)))
    {
        return Err(StoreError::Unsupported("child.orchestration"));
    }
    let link = link(c, &parent.id)?;
    let (root, depth) = link.map_or((parent.id.clone(), 1), |l| (l.root_task_id, l.depth + 1));
    let root_task = orchestration::task(c, &root)?;
    let root_policy = root_task
        .spec
        .orchestration
        .as_ref()
        .ok_or(StoreError::Database)?;
    let total: u32 = c
        .query_row(
            "SELECT count(*) FROM child_links WHERE root_task_id=?1",
            [&root],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let direct: u32 = c
        .query_row(
            "SELECT count(*) FROM child_links WHERE parent_task_id=?1",
            [&parent.id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if depth > root_policy.max_depth
        || total >= root_policy.max_children
        || direct >= policy.max_children
    {
        return Err(StoreError::Limit);
    }
    orchestration::validate_spec(spec)?;
    Ok((root, depth))
}

fn selected_context(
    c: &Connection,
    parent: &TaskResponse,
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
                "SELECT session_id,status,text FROM messages WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)?;
        if session != parent.latest_run.session_id || status != "completed" {
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
        let owned:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM tool_invocations i JOIN turns t ON t.id=i.turn_id JOIN json_each(i.outcome,'$.artifacts') a WHERE t.session_id=?1 AND json_extract(a.value,'$.id')=?2)",rusqlite::params![parent.latest_run.session_id,id],|r|r.get(0)).map_err(|_|StoreError::Database)?;
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
            .take((slop_runtime::tools::PREVIEW_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| StoreError::Unsupported("child.artifact_unavailable"))?;
        let public = String::from_utf8_lossy(&bytes);
        let preview = bounded_text(&public, slop_runtime::tools::PREVIEW_BYTES);
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
    let limit = (slop_runtime::tools::PREVIEW_BYTES / ids.len().max(1)).saturating_sub(400);
    let mut values = Vec::new();
    for id in ids {
        let run = orchestration::run(c, id)?;
        let output = run
            .turn
            .assistant_message_id
            .as_deref()
            .map(|id| orchestration::message(c, id))
            .transpose()?;
        let text = output.as_ref().map(|m| bounded_text(&m.text, limit));
        values.push(serde_json::json!({"run_id":run.id,"task_id":run.task_id,"status":run.turn.status,"error_code":run.turn.error_code,"effects_unknown":run.effects_unknown,"assistant_message_id":run.turn.assistant_message_id,"text":text,"output_truncated":output.as_ref().is_some_and(|m|m.text.len()>limit)}));
    }
    let mut encoded = canonical(&values)?;
    if encoded.len() > slop_runtime::tools::PREVIEW_BYTES {
        for value in &mut values {
            value["text"] = serde_json::Value::Null;
            value["output_truncated"] = serde_json::Value::Bool(true);
        }
        encoded = canonical(&values)?;
    }
    if encoded.len() > slop_runtime::tools::PREVIEW_BYTES {
        return Err(StoreError::Limit);
    }
    Ok(encoded)
}

fn direct_child(c: &Connection, parent_run: &str, child_task: &str) -> StoreResult<TaskResponse> {
    let matched: bool = c
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM child_links WHERE parent_run_id=?1 AND child_task_id=?2)",
            rusqlite::params![parent_run, child_task],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if !matched {
        return Err(StoreError::NotFound);
    }
    orchestration::task(c, child_task)
}

fn accept_wait(
    tx: &Transaction<'_>,
    parent_run: &str,
    request: &WaitChildrenRequest,
    intent: Option<&ToolIntent>,
) -> StoreResult<()> {
    let parent = parent(tx, parent_run)?;
    if request.child_run_ids.is_empty()
        || request.child_run_ids.len() > 32
        || request
            .child_run_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != request.child_run_ids.len()
    {
        return Err(StoreError::Invalid);
    }
    for id in &request.child_run_ids {
        let run = orchestration::run(tx, id)?;
        direct_child(tx, parent_run, &run.task_id)?;
    }
    let encoded = canonical(&request.child_run_ids)?;
    let existing: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT parent_turn_id,invocation_id,child_run_ids FROM child_waits WHERE id=?1",
            [&request.command_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    if let Some((turn, invocation, ids)) = existing {
        if turn != parent.latest_run.turn.id
            || invocation.as_deref() != intent.map(|i| i.id.as_str())
            || ids != encoded
        {
            return Err(StoreError::Conflict);
        }
        return wake(tx);
    }
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM child_waits WHERE parent_turn_id=?1 AND completed=0)",
            [&parent.latest_run.turn.id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let total: u32 = tx
        .query_row(
            "SELECT count(*) FROM child_waits WHERE parent_turn_id=?1",
            [&parent.latest_run.turn.id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if pending || total >= 64 {
        return Err(StoreError::Conflict);
    }
    tx.execute("INSERT INTO child_waits(id,parent_turn_id,invocation_id,child_run_ids) VALUES(?1,?2,?3,?4)",rusqlite::params![request.command_id,parent.latest_run.turn.id,intent.map(|i|&i.id),encoded]).map_err(|_|StoreError::Database)?;
    if let Some(intent) = intent {
        tx.execute(
            "UPDATE tool_invocations SET status='waiting' WHERE id=?1",
            [&intent.id],
        )
        .map_err(|_| StoreError::Database)?;
    }
    append_event(
        tx,
        &parent.latest_run.session_id,
        "child_wait_accepted",
        Some(&parent.latest_run.turn.id),
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

/// A completed child only wakes a known continuation. Never dispatch a tool here.
pub(super) fn wake(tx: &Transaction<'_>) -> StoreResult<()> {
    let mut stmt=tx.prepare("SELECT id,parent_turn_id,invocation_id,child_run_ids FROM child_waits WHERE completed=0").map_err(|_|StoreError::Database)?;
    let waits = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                execution::json_column::<Vec<String>>(r, 3)?,
            ))
        })
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(stmt);
    for (id, turn, invocation, ids) in waits {
        let all:bool=tx.query_row("SELECT NOT EXISTS(SELECT 1 FROM json_each(?1) j JOIN runs r ON r.id=j.value JOIN turns t ON t.id=r.turn_id WHERE t.status IN('queued','running','paused','awaiting_children'))",[canonical(&ids)?],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        if !all {
            continue;
        }
        let session: String = tx
            .query_row("SELECT session_id FROM turns WHERE id=?1", [&turn], |r| {
                r.get(0)
            })
            .map_err(|_| StoreError::Database)?;
        if let Some(invocation) = invocation {
            let intent = tool_intent(tx, &invocation)?;
            execution::finish_tool_tx(
                tx,
                &session,
                &turn,
                &intent,
                &completed(summaries(tx, &ids)?),
            )?;
        }
        tx.execute("UPDATE child_waits SET completed=1 WHERE id=?1", [id])
            .map_err(|_| StoreError::Database)?;
        tx.execute(
            "UPDATE turns SET status='queued' WHERE id=?1 AND status='awaiting_children'",
            [&turn],
        )
        .map_err(|_| StoreError::Database)?;
        append_event(tx, &session, "child_wait_completed", Some(&turn), None)?;
    }
    Ok(())
}

pub(super) fn park(tx: &Transaction<'_>, turn: &str) -> StoreResult<()> {
    let status = if pending(tx, turn)? {
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
    let mut stmt=tx.prepare("SELECT t.id FROM child_links l JOIN runs parent ON parent.id=l.parent_run_id JOIN runs r ON r.task_id=l.child_task_id JOIN turns t ON t.id=r.turn_id WHERE parent.turn_id=?1 AND l.cancel_with_parent=1 AND t.status IN('queued','running','paused','awaiting_children')").map_err(|_|StoreError::Database)?;
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

pub(super) fn retry_allowed(c: &Connection, task: &TaskResponse) -> StoreResult<()> {
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
            "SELECT admission_batch_id FROM tasks WHERE id=?1",
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
        effective: Option<TaskSpec>,
    ) -> StoreResult<TaskReceipt> {
        self.create_child_inner(parent_run, request, effective, None)
            .await
    }

    async fn create_child_inner(
        &self,
        parent_run: &str,
        request: CreateChildRequest,
        effective: Option<TaskSpec>,
        intent: Option<ToolIntent>,
    ) -> StoreResult<TaskReceipt> {
        if !valid_command_id(&request.command_id) {
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
        let parent_snapshot = self.submit(move |c| parent(c, &lookup)).await?;
        let mut spec = effective.ok_or(StoreError::Invalid)?;
        // Freeze inherited project identity before Git I/O, then revalidate all
        // limits in the acceptance transaction to handle competing creates.
        let mut project = spec.project.clone();
        if let (Some(p), Some(child)) = (&parent_snapshot.spec.project, &mut project) {
            if p.project_id != child.project_id
                || child
                    .base_ref
                    .as_ref()
                    .is_some_and(|b| Some(b) != p.base_ref.as_ref())
            {
                return Err(StoreError::Unsupported("child.project"));
            }
            child.base_ref = p.base_ref.clone();
        }
        spec.project = project;
        let resolved = self.resolve_job_project(&mut spec.project).await?;
        let artifact_dir = self.shared.data_dir.join("artifacts");
        self.submit(move|c| {
            let tx=c.unchecked_transaction().map_err(|_|StoreError::Database)?;
            if let Some(receipt)=projects::prior(&tx,&request.command_id,&scope,&payload)? {return Ok(receipt);}
            let parent=parent(&tx,&parent_id)?;
            let (root,depth)=constrain(&tx,&parent,&mut spec)?;
            spec.prompt=selected_context(&tx,&parent,&request.context,&spec.prompt,&artifact_dir)?;
            let receipt=orchestration::insert_task(&tx,&spec,&request.spec,resolved,None,&request.command_id)?;
            tx.execute("INSERT INTO child_links(child_task_id,parent_task_id,parent_run_id,root_task_id,depth,cancel_with_parent,context) VALUES(?1,?2,?3,?4,?5,?6,?7)",rusqlite::params![receipt.task_id,parent.id,parent_id,root,depth,request.cancel_with_parent,canonical(&request.context)?]).map_err(|_|StoreError::Database)?;
            tx.execute("UPDATE tasks SET root_task_id=?1,admission_batch_id=(SELECT admission_batch_id FROM tasks WHERE id=?2) WHERE id=?3",rusqlite::params![root,parent.id,receipt.task_id]).map_err(|_|StoreError::Database)?;
            append_event(&tx,&parent.latest_run.session_id,"child_created",Some(&parent.latest_run.turn.id),None)?;
            projects::save(&tx,&request.command_id,&scope,&payload,&receipt)?;
            if let Some(intent)=intent {execution::finish_tool_tx(&tx,&parent.latest_run.session_id,&parent.latest_run.turn.id,&intent,&completed(canonical(&receipt)?))?;}
            tx.commit().map_err(|_|StoreError::Database)?;
            Ok(receipt)
        }).await
    }

    pub async fn children(
        &self,
        id: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<TaskResponse>> {
        let id = id.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move|c| {orchestration::run(c,&id)?;orchestration::page(c,"SELECT k.id,k.created_order FROM tasks k JOIN child_links l ON l.child_task_id=k.id WHERE l.parent_run_id=?1 AND k.created_order>?2 ORDER BY k.created_order LIMIT ?3",Some(&id),after,limit,orchestration::task)}).await
    }

    pub async fn run_result(&self, id: &str) -> StoreResult<RunResultResponse> {
        let id = id.to_owned();
        self.submit(move |c| {
            let run = orchestration::run(c, &id)?;
            let output = run
                .turn
                .assistant_message_id
                .as_deref()
                .map(|id| orchestration::message(c, id))
                .transpose()?;
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
            if let Some(receipt) = projects::prior(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let parent = parent(&tx, &id)?;
            accept_wait(&tx, &id, &request, None)?;
            if parent.latest_run.turn.status == "queued"
                && pending(&tx, &parent.latest_run.turn.id)?
            {
                park(&tx, &parent.latest_run.turn.id)?;
            }
            let current = get_session(&tx, &parent.latest_run.session_id)?;
            let receipt = make_receipt(
                &request.command_id,
                &current.id,
                Some(parent.latest_run.turn.id),
                None,
                current.revision,
                current.last_event_sequence,
            );
            projects::save(&tx, &request.command_id, &scope, &payload, &receipt)?;
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
                c.query_row("SELECT id FROM runs WHERE turn_id=?1", [lookup], |r| {
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
            let p = self.task(&self.run(&run_id).await?.task_id).await?;
            if effective.settings.max_output_tokens.is_none()
                && !effective.model.starts_with("codex/")
            {
                effective.settings.max_output_tokens = p.spec.settings.max_output_tokens;
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
            let result=if intent.name=="child_wait" {
                let ids:Vec<String>=serde_json::from_value(intent.arguments["child_run_ids"].clone()).map_err(|_|StoreError::Invalid)?;
                accept_wait(&tx,&run_id,&WaitChildrenRequest{command_id:format!("wait-{}",intent.id),child_run_ids:ids.clone()},Some(&intent))?;
                if pending(&tx,&turn_id)? {OrchestrationResult::Waiting} else {OrchestrationResult::Completed(completed(summaries(&tx,&ids)?))}
            } else {
                let task_id=intent.arguments["task_id"].as_str().ok_or(StoreError::Invalid)?;
                let child=direct_child(&tx,&run_id,task_id)?;
                let output=match intent.name.as_str() {
                    "child_inspect"=>canonical(&serde_json::json!({"task_id":child.id,"run_id":child.latest_run.id,"status":child.latest_run.turn.status,"workspace_id":child.latest_run.workspace_id,"model":child.spec.model,"budget_usage":child.budget_usage}))?,
                    "child_result"=>{
                        if matches!(child.latest_run.turn.status.as_str(),"queued"|"running"|"paused"|"awaiting_children") {return Err(StoreError::Conflict);}
                        summaries(&tx,&[child.latest_run.id])?
                    },
                    "child_cancel"=>canonical(&cancel_turn_tx(&tx,&child.latest_run.turn.id,CancelTurnRequest{command_id:format!("cancel-{}",intent.id)})?)?,
                    _=>return Err(StoreError::Invalid),
                };
                let outcome=completed(output);
                execution::finish_tool_tx(&tx,&parent.latest_run.session_id,&turn_id,&intent,&outcome)?;
                OrchestrationResult::Completed(outcome)
            };
            tx.commit().map_err(|_|StoreError::Database)?;Ok(result)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_runtime::{
        agent::{RequestCompletion, RequestIntent},
        chat::{ChatOutcome, ChatRepository, ChatStatus},
        providers::{Usage, inference::*},
    };

    async fn open(data: PathBuf) -> Store {
        Store::open(data, None, 64, Duration::from_secs(1))
            .await
            .unwrap()
    }
    fn spec(prompt: &str) -> TaskSpec {
        TaskSpec {
            title: None,
            prompt: prompt.into(),
            model: "opencode-go/glm-5.3-flash".into(),
            settings: slop_protocol::execution::GenerationSettings {
                max_output_tokens: Some(64),
                reasoning_effort: None,
            },
            project: None,
            budget: Some(OperationBudget {
                max_model_requests: 32,
                max_tool_calls: 64,
            }),
            orchestration: None,
        }
    }
    fn policy() -> slop_protocol::orchestration::OrchestrationPolicy {
        slop_protocol::orchestration::OrchestrationPolicy {
            max_children: 4,
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
        c.submit(|db|db.execute_batch("DROP TABLE child_waits; DROP TABLE child_links; DROP TABLE admission_groups; DROP TABLE admission_clock; DROP TABLE operation_budgets; DROP TRIGGER readmission_epoch; ALTER TABLE turns DROP COLUMN admission_epoch; ALTER TABLE turns DROP COLUMN budget_exhausted; ALTER TABLE tasks DROP COLUMN root_task_id; ALTER TABLE tasks DROP COLUMN admission_batch_id; ALTER TABLE batches DROP COLUMN cancellation_requested; PRAGMA user_version=6;").map_err(|_|StoreError::Database)).await.unwrap();
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
        assert_eq!(task.latest_run.turn.status, "completed");
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
            4
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
            project: None,
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
    async fn cancellation_and_budgets_cover_descendants_without_expanding_authority() {
        let dir = tempfile::tempdir().unwrap();
        let store = open(dir.path().join("data")).await;
        let c = store.client();
        let parent = root(&c, "tree-parent").await;
        let mut request = child("branch");
        let mut delegated = policy();
        delegated.max_depth = 1;
        request.spec.orchestration = Some(delegated);
        let branch = c
            .create_child(&parent.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
        let request = child("leaf");
        let leaf = c
            .create_child(&branch.run_id, request.clone(), Some(request.spec))
            .await
            .unwrap();
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
        for child in [&branch, &leaf] {
            assert_eq!(c.run(&child.run_id).await.unwrap().turn.status, "cancelled");
        }
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
