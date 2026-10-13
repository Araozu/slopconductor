use super::*;

pub(super) fn get_base_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    connection.query_row("SELECT id,session_id,user_message_id,assistant_message_id,status,requested_model,resolved_model,input_tokens,output_tokens,total_tokens,total_source,error_code,error_message FROM turns WHERE id=?1",[id],|r|{let input:Option<String>=r.get(7)?;let output:Option<String>=r.get(8)?;let total:Option<String>=r.get(9)?;let total_source:Option<String>=r.get(10)?;Ok(TurnResponse{id:r.get(0)?,session_id:r.get(1)?,user_message_id:r.get(2)?,assistant_message_id:r.get(3)?,status:r.get(4)?,requested_model:r.get(5)?,resolved_model:r.get(6)?,usage:if input.is_some()||output.is_some()||total.is_some()||total_source.is_some(){Some(UsageResponse{input_tokens:input.and_then(|v|v.parse().ok()),output_tokens:output.and_then(|v|v.parse().ok()),total_tokens:total.and_then(|v|v.parse().ok()),total_source})}else{None},error_code:r.get(11)?,error_message:r.get(12)?,settings:Default::default(),model_request_ids:Vec::new(),tool_invocation_ids:Vec::new()})}).optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)
}

pub(super) fn cancel_turn(
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
    let receipt = cancel_turn_tx(&tx, turn_id, request)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

pub(super) fn cancel_turn_tx(
    tx: &Transaction<'_>,
    turn_id: &str,
    request: CancelTurnRequest,
) -> StoreResult<CommandReceipt> {
    let scope = format!("turn:{turn_id}:cancel");
    let payload = canonical(&request)?;
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
        "queued" | "paused" | "awaiting_children" => {
            tx.execute("UPDATE turns SET status='cancelled' WHERE id=?1", [turn_id])
                .map_err(|_| StoreError::Database)?;
            reject_steering_tx(tx, turn_id, "turn_cancelled")?;
            execution::reconcile_tools(tx, turn_id, "turn_cancelled")?;
            tx.execute(
                "UPDATE child_waits SET completed=1 WHERE parent_turn_id=?1",
                [turn_id],
            )
            .map_err(|_| StoreError::Database)?;
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
    let (revision, event_sequence) = append_event(tx, &session_id, kind, Some(turn_id), None)?;
    let receipt = make_receipt(
        &request.command_id,
        &session_id,
        Some(turn_id.to_owned()),
        None,
        revision,
        event_sequence,
    );
    commit_command(tx, &request.command_id, &scope, &payload, &receipt)?;
    children::cancel_descendants(tx, turn_id)?;
    children::wake(tx)?;
    Ok(receipt)
}

pub(super) fn recover_interrupted(connection: &Connection) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    children::recover_waits(&tx)?;
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
    children::wake(&tx)?;
    projects::recover(&tx)?;
    tx.commit().map_err(|_| StoreError::Database)
}

pub(super) fn turn_control(
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
        ("pause", "queued" | "awaiting_children") => {
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
            if children::pending(&tx, turn_id)? {
                tx.execute(
                    "UPDATE turns SET status='awaiting_children' WHERE id=?1",
                    [turn_id],
                )
                .map_err(|_| StoreError::Database)?;
            }
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

pub(super) fn list_steering(
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

pub(super) fn turn_control_state(
    connection: &Connection,
    turn_id: &str,
) -> StoreResult<slop_runtime::chat::TurnControl> {
    connection
        .query_row(
            "SELECT cancellation_requested,immediate_requested,pause_requested,EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='accepted'),budget_exhausted,EXISTS(SELECT 1 FROM child_waits w WHERE w.parent_turn_id=?1 AND w.completed=0) FROM turns WHERE id=?1",
            [turn_id],
            |row| {
                Ok(slop_runtime::chat::TurnControl {
                    cancellation_requested: row.get::<_, i64>(0)? != 0,
                    immediate_requested: row.get::<_, i64>(1)? != 0,
                    pause_requested: row.get::<_, i64>(2)? != 0,
                    steering_pending: row.get(3)?,
                    budget_exhausted: row.get(4)?,
                    waiting_requested: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)
}

pub(super) fn apply_steering_tx(tx: &Transaction<'_>, turn_id: &str) -> StoreResult<usize> {
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
    let model_ref = messages::turn_model(tx, turn_id)?;
    for (id, text) in &instructions {
        let ordinal = execution::next_message_ordinal(tx, turn_id)?;
        tx.execute(
            "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,provider,model) VALUES(?1,?2,?3,?4,'user',?5,'completed',?6,?7)",
            rusqlite::params![id, session_id, turn_id, ordinal, text,model_ref.provider().as_str(),model_ref.model()],
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

pub(super) fn reject_steering_tx(
    tx: &Transaction<'_>,
    turn_id: &str,
    cause: &str,
) -> StoreResult<()> {
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

pub(super) fn claim_next_turn(
    connection: &Connection,
) -> StoreResult<Option<slop_runtime::chat::TurnWork>> {
    execution::claim_next_turn(connection)
}

pub(super) fn get_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    execution::get_turn(connection, id)
}

pub(super) fn checkpoint_turn(
    connection: &Connection,
    turn_id: &str,
    text: &str,
) -> StoreResult<()> {
    if text.len() > MAX_OUTPUT_BYTES {
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
        let model_ref = messages::turn_model(&tx, turn_id)?;
        tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,provider,model) VALUES(?1,?2,?3,?4,'assistant',?5,'checkpoint',?6,?7)",rusqlite::params![id,session_id,turn_id,ordinal,text,model_ref.provider().as_str(),model_ref.model()]).map_err(|_|StoreError::Database)?;
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

pub(super) fn finish_turn(
    connection: &Connection,
    turn_id: &str,
    mut outcome: slop_runtime::chat::ChatOutcome,
) -> StoreResult<()> {
    use slop_runtime::chat::ChatStatus;
    if outcome.text.len() > MAX_OUTPUT_BYTES {
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
    if already_unknown {
        outcome.usage = slop_runtime::providers::Usage::default();
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
        Some("steering_resume" | "steering_resume_unknown" | "context_compaction_resume")
    ) || pending_steering && outcome.status == ChatStatus::Completed;
    let pause_yield = outcome.error_code.as_deref() == Some("pause_turn")
        || pause_requested
            && (outcome.status == ChatStatus::Completed
                || outcome.error_code.as_deref() == Some("awaiting_children"));
    let wait_yield = outcome.error_code.as_deref() == Some("awaiting_children")
        || children::pending(&tx, turn_id)? && outcome.status == ChatStatus::Completed;
    if !cancel_requested && !pause_requested && wait_yield {
        execution::reconcile_requests(&tx, turn_id, "awaiting_children")?;
        execution::reconcile_tools(&tx, turn_id, "awaiting_children")?;
        apply_steering_tx(&tx, turn_id)?;
        let usage = outcome.usage;
        tx.execute("UPDATE turns SET input_tokens=?1,output_tokens=?2,total_tokens=?3,total_source=?4 WHERE id=?5",rusqlite::params![usage.input_tokens.map(|n|n.to_string()),usage.output_tokens.map(|n|n.to_string()),usage.total_tokens.map(|n|n.to_string()),usage.total_source.map(|s|match s {slop_runtime::providers::UsageSource::Reported=>"reported",slop_runtime::providers::UsageSource::Derived=>"derived"}),turn_id]).map_err(|_|StoreError::Database)?;
        children::park(&tx, turn_id)?;
        children::wake(&tx)?;
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(());
    }
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
                if outcome.error_code.as_deref() == Some("context_compaction_resume") {
                    "turn_context_yielded"
                } else {
                    "turn_steering_yielded"
                }
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
            let model_ref = messages::turn_model(&tx, turn_id)?;
            tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,provider,model) VALUES(?1,?2,?3,?4,'assistant',?5,?6,?7,?8)",rusqlite::params![id,session_id,turn_id,ordinal,outcome.text,message_status,model_ref.provider().as_str(),outcome.resolved_model.as_deref().unwrap_or(model_ref.model())]).map_err(|_|StoreError::Database)?;
            id
        };
        // Structured messages retain their own request's resolved identity.
        tx.execute(
            "UPDATE messages SET model=COALESCE(?1,model) WHERE id=?2 AND request_id IS NULL",
            rusqlite::params![outcome.resolved_model, id],
        )
        .map_err(|_| StoreError::Database)?;
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
    children::wake(&tx)?;
    tx.commit().map_err(|_| StoreError::Database)
}

pub(super) fn cancellation_requested(connection: &Connection, turn_id: &str) -> StoreResult<bool> {
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
