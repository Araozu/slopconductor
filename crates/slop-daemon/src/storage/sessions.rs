use super::*;

pub(super) fn create_session(
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

pub(super) fn create_session_tx(
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
        || request
            .draft_id
            .as_ref()
            .is_some_and(|id| !valid_command_id(id) || request.prompt.is_none())
    {
        return Err(StoreError::Invalid);
    }
    governance::validate(request.budget.as_ref())?;
    let requested = slop_protocol::orchestration::SessionSpec {
        title: request.title.clone(),
        prompt: request.prompt.clone().unwrap_or_default(),
        model: format!("{}/{}", request.provider, request.model),
        settings: request.settings.clone().unwrap_or_default(),
        project: request.project.clone(),
        budget: request.budget.clone(),
        orchestration: request.orchestration.clone(),
    };
    children::validate_policy(&requested)?;
    if request.prompt.is_some() {
        orchestration::validate_spec(&requested)?;
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
        .filter(|_| {
            resolved
                .as_ref()
                .is_some_and(|r| !r.project.git_common_dir.is_empty())
        })
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
    } else if let (Some(selection), Some(resolved)) = (&request.project, &resolved) {
        let policy = slop_runtime::tools::WorkspacePolicy {
            root: resolved.project.path.clone(),
            allowed_tools: selection.allowed_tools.clone(),
            shell_timeout_ms: 30_000,
            max_output_bytes: 1024 * 1024,
            max_tool_calls: u32::MAX,
            max_model_requests: u32::MAX,
        };
        Some(policy.canonicalize().map_err(|_| StoreError::Invalid)?)
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
    let mut spec = requested.clone();
    spec.settings = settings;
    if let Some(resolved) = &resolved
        && let Some(project) = &mut spec.project
        && !resolved.base_commit.is_empty()
    {
        project.base_ref = Some(resolved.base_commit.clone());
    }
    tx.execute(
        "UPDATE sessions SET spec=?1,requested_spec=?2,root_task_id=id WHERE id=?3",
        rusqlite::params![canonical(&spec)?, canonical(&requested)?, session_id],
    )
    .map_err(|_| StoreError::Database)?;
    governance::register(tx, &format!("session:{session_id}"), spec.budget.as_ref())?;
    let (mut revision, mut sequence) =
        append_event(tx, &session_id, "session_created", None, None)?;
    if let Some((workspace, _)) = managed {
        (revision, sequence) = projects::insert_workspace(tx, &workspace)?;
    }
    let mut receipt = make_receipt(
        &request.command_id,
        &session_id,
        None,
        None,
        revision,
        sequence,
    );
    if let Some(prompt) = request.prompt {
        let initial = send_message_tx(
            tx,
            &session_id,
            SendMessageRequest {
                command_id: opaque_id()?,
                text: prompt,
                settings: request.settings,
                ..Default::default()
            },
            Some(spec.settings),
            None,
        )?;
        receipt = make_receipt(
            &request.command_id,
            &session_id,
            initial.turn_id,
            initial.message_id,
            initial.revision,
            initial.event_sequence,
        );
    }
    commit_command(tx, &request.command_id, scope, &payload, &receipt)?;
    if let Some(id) = request.draft_id {
        tx.execute("DELETE FROM conversation_drafts WHERE id=?1", [id])
            .map_err(|_| StoreError::Database)?;
    }
    Ok(receipt)
}

pub(super) fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionResponse> {
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
        spec: execution::json_column(row, 11)?,
        requested_spec: execution::json_column(row, 12)?,
        batch_id: row.get(13)?,
        combination_index: row.get(14)?,
        latest_run: None,
        budget_usage: Default::default(),
        child: None,
        inbox: execution::optional_json_column(row, 15)?,
        fork: execution::optional_json_column(row, 16)?,
    })
}

pub(super) fn get_session(connection: &Connection, id: &str) -> StoreResult<SessionResponse> {
    let session = connection.query_row("SELECT id,owner_node_id,title,provider,model,max_tokens,revision,last_event_sequence,settings,execution,managed_workspace_id,spec,requested_spec,batch_id,combination_index,inbox,fork FROM sessions WHERE id=?1",[id],session_from_row)
        .optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)?;
    enrich(connection, session)
}

pub(super) fn list_sessions(
    connection: &Connection,
    after: Option<u64>,
    limit: usize,
) -> StoreResult<Page<SessionResponse>> {
    let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT id,owner_node_id,title,provider,model,max_tokens,revision,last_event_sequence,settings,execution,managed_workspace_id,spec,requested_spec,batch_id,combination_index,inbox,fork,created_order FROM sessions WHERE created_order>?1 ORDER BY created_order LIMIT ?2")
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
        let item = enrich(
            connection,
            session_from_row(row).map_err(|_| StoreError::Database)?,
        )?;
        next_after = Some(row.get::<_, i64>(17).map_err(|_| StoreError::Database)? as u64);
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

fn enrich(connection: &Connection, mut session: SessionResponse) -> StoreResult<SessionResponse> {
    let latest: Option<String> = connection
        .query_row(
            "SELECT id FROM turns t WHERE session_id=?1 AND EXISTS(SELECT 1 FROM messages m WHERE m.turn_id=t.id AND m.deleted=0) ORDER BY ordinal DESC LIMIT 1",
            [&session.id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    session.latest_run = latest
        .map(|id| orchestration::run(connection, &id))
        .transpose()?;
    session.budget_usage = governance::usage(connection, &format!("session:{}", session.id))?;
    session.child = children::link(connection, &session.id)?;
    Ok(session)
}
