use super::*;

pub(super) fn send_message(
    connection: &Connection,
    session_id: &str,
    request: SendMessageRequest,
    effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    idle_failure: Option<(&'static str, &'static str)>,
) -> StoreResult<CommandReceipt> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let receipt = send_message_tx(&tx, session_id, request, effective_settings, idle_failure)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}

pub(super) fn send_message_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    request: SendMessageRequest,
    effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    idle_failure: Option<(&'static str, &'static str)>,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id)
        || request.text.trim().is_empty()
        || request.text.len() > MAX_MESSAGE_BYTES
        || request.text.chars().any(|c| c == '\0')
    {
        return Err(StoreError::Invalid);
    }
    let scope = format!("session:{session_id}:message");
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(tx, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let session = get_session(tx, session_id)?;
    projects::require_available(tx, &session)?;
    if request
        .expected_revision
        .is_some_and(|revision| revision != session.revision)
    {
        return Err(StoreError::Conflict);
    }
    let delivery = request.delivery.unwrap_or_default();
    if delivery != DeliveryMode::AfterTurn {
        let active_turn: Option<String> = tx
            .query_row(
                "SELECT id FROM turns t WHERE session_id=?1 AND (status='running' OR status='paused' OR status='awaiting_children' OR (status='queued' AND EXISTS(SELECT 1 FROM model_requests r WHERE r.turn_id=t.id))) ORDER BY ordinal LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| StoreError::Database)?;
        if let Some(active_turn) = active_turn {
            if request.model.is_some() || request.settings.is_some() {
                return Err(StoreError::ActiveTurnOverride);
            }
            let pending: i64 = tx
                .query_row(
                    "SELECT count(*) FROM steering_instructions WHERE turn_id=?1 AND status='accepted'",
                    [&active_turn],
                    |row| row.get(0),
                )
                .map_err(|_| StoreError::Database)?;
            if pending >= MAX_PENDING_STEERING_PER_TURN {
                return Err(StoreError::Limit);
            }
            let instruction_id = opaque_id()?;
            tx.execute(
                "INSERT INTO steering_instructions(id,command_id,session_id,turn_id,text,delivery,status,created_order) VALUES(?1,?2,?3,?4,?5,?6,'accepted',(SELECT COALESCE(MAX(created_order),0)+1 FROM steering_instructions))",
                rusqlite::params![
                    instruction_id,
                    request.command_id,
                    session_id,
                    active_turn,
                    request.text,
                    match delivery {
                        DeliveryMode::NextBoundary => "next-boundary",
                        DeliveryMode::Immediate => "immediate",
                        DeliveryMode::AfterTurn => unreachable!(),
                    }
                ],
            )
            .map_err(|_| StoreError::Database)?;
            if delivery == DeliveryMode::Immediate {
                tx.execute(
                    "UPDATE turns SET immediate_requested=1 WHERE id=?1 AND status='running'",
                    [&active_turn],
                )
                .map_err(|_| StoreError::Database)?;
            }
            let (revision, event_sequence) = append_event(
                tx,
                session_id,
                "steering_instruction_accepted",
                Some(&active_turn),
                Some(&instruction_id),
            )?;
            let receipt = make_receipt(
                &request.command_id,
                session_id,
                Some(active_turn),
                Some(instruction_id),
                revision,
                event_sequence,
            );
            commit_command(tx, &request.command_id, &scope, &payload, &receipt)?;
            return Ok(receipt);
        }
    }
    if let Some((code, message)) = idle_failure {
        return Err(StoreError::Preflight(code, message));
    }
    let queued_session: i64 = tx
        .query_row(
            "SELECT count(*) FROM turns WHERE session_id=?1 AND status='queued'",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let queued_global: i64 = tx
        .query_row(
            "SELECT count(*) FROM turns WHERE status='queued'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if queued_session >= MAX_QUEUE_PER_SESSION || queued_global >= MAX_QUEUE_GLOBAL {
        return Err(StoreError::Limit);
    }
    let turn_id = opaque_id()?;
    let message_id = opaque_id()?;
    let ordinal: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(ordinal),0)+1 FROM turns WHERE session_id=?1",
            [session_id],
            |r| r.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let user_order = ordinal.checked_mul(1024).ok_or(StoreError::Limit)?;
    let requested_model = request
        .model
        .clone()
        .unwrap_or_else(|| format!("{}/{}", session.provider, session.model));
    let settings = match effective_settings {
        Some(settings) => {
            execution::validate_settings(&requested_model, &settings)?;
            settings
        }
        None => execution::turn_settings(&session, &request, &requested_model)?,
    };
    tx.execute("INSERT INTO turns(id,session_id,ordinal,user_message_id,status,requested_model) VALUES(?1,?2,?3,?4,'queued',?5)",rusqlite::params![turn_id,session_id,ordinal,message_id,requested_model]).map_err(|_|StoreError::Database)?;
    tx.execute(
        "UPDATE turns SET settings=?1,requested_settings=?2 WHERE id=?3",
        rusqlite::params![
            canonical(&settings)?,
            canonical(&request.settings.clone().unwrap_or_default())?,
            turn_id
        ],
    )
    .map_err(|_| StoreError::Database)?;
    let mut spec = session.spec;
    let mut requested = session.requested_spec;
    if session.latest_run.is_some() || spec.prompt.is_empty() {
        spec.prompt = request.text.clone();
        requested.prompt = request.text.clone();
    }
    spec.model = requested_model.clone();
    spec.settings = settings;
    requested.model = requested_model;
    requested.settings = request.settings.clone().unwrap_or_default();
    tx.execute(
        "UPDATE sessions SET spec=?1,requested_spec=?2 WHERE id=?3",
        rusqlite::params![canonical(&spec)?, canonical(&requested)?, session_id],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'user',?5,'completed')",rusqlite::params![message_id,session_id,turn_id,user_order,request.text]).map_err(|_|StoreError::Database)?;
    let (revision, event_sequence) = append_event(
        tx,
        session_id,
        "user_message_accepted",
        Some(&turn_id),
        Some(&message_id),
    )?;
    let receipt = make_receipt(
        &request.command_id,
        session_id,
        Some(turn_id),
        Some(message_id),
        revision,
        event_sequence,
    );
    commit_command(tx, &request.command_id, &scope, &payload, &receipt)?;
    Ok(receipt)
}

pub(super) fn list_messages(
    connection: &Connection,
    session_id: &str,
    after: Option<u64>,
    limit: usize,
) -> StoreResult<Page<MessageResponse>> {
    let _ = get_session(connection, session_id)?;
    let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT id,session_id,turn_id,role,text,status,blocks,request_id,ordinal FROM messages WHERE session_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3").map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![session_id, after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    let mut encoded_bytes = 128usize;
    let mut has_more = false;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            has_more = true;
            break;
        }
        let item = MessageResponse {
            id: row.get(0).map_err(|_| StoreError::Database)?,
            session_id: row.get(1).map_err(|_| StoreError::Database)?,
            turn_id: row.get(2).map_err(|_| StoreError::Database)?,
            role: row.get(3).map_err(|_| StoreError::Database)?,
            text: row.get(4).map_err(|_| StoreError::Database)?,
            status: row.get(5).map_err(|_| StoreError::Database)?,
            blocks: execution::public_blocks_from_row(row, 6, 0, 4)?,
            request_id: row.get(7).map_err(|_| StoreError::Database)?,
        };
        let item_bytes = serde_json::to_vec(&item)
            .map_err(|_| StoreError::Database)?
            .len();
        if encoded_bytes.saturating_add(item_bytes).saturating_add(1) > MAX_MESSAGE_PAGE_BYTES {
            if items.is_empty() {
                return Err(StoreError::Limit);
            }
            has_more = true;
            break;
        }
        encoded_bytes += item_bytes + usize::from(!items.is_empty());
        next_after = Some(row.get::<_, i64>(8).map_err(|_| StoreError::Database)? as u64);
        items.push(item);
    }
    if has_more {
        // Keep the cursor after the last included row. The next page may fit a
        // large message that was deferred solely by the encoded-size bound.
    } else if next_after.is_some_and(|cursor| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE session_id=?1 AND ordinal>?2)",
                rusqlite::params![session_id, cursor as i64],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }) {
        has_more = true;
    }
    if !has_more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}

pub(super) fn list_events(
    connection: &Connection,
    session_id: &str,
    after: u64,
    limit: usize,
) -> StoreResult<Page<EventResponse>> {
    let _ = get_session(connection, session_id)?;
    let after = after.min(i64::MAX as u64) as i64;
    let mut statement=connection.prepare("SELECT session_id,sequence,kind,turn_id,message_id,revision,request_id,invocation_id FROM session_events WHERE session_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3").map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![session_id, after, (limit + 1) as i64])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut next_after = None;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            break;
        }
        let sequence: i64 = row.get(1).map_err(|_| StoreError::Database)?;
        items.push(EventResponse {
            session_id: row.get(0).map_err(|_| StoreError::Database)?,
            sequence: sequence as u64,
            kind: row.get(2).map_err(|_| StoreError::Database)?,
            turn_id: row.get(3).map_err(|_| StoreError::Database)?,
            message_id: row.get(4).map_err(|_| StoreError::Database)?,
            revision: row.get::<_, i64>(5).map_err(|_| StoreError::Database)? as u64,
            request_id: row.get(6).map_err(|_| StoreError::Database)?,
            invocation_id: row.get(7).map_err(|_| StoreError::Database)?,
        });
        next_after = Some(sequence as u64);
    }
    let more = next_after.is_some_and(|cursor| {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM session_events WHERE session_id=?1 AND sequence>?2)",
                rusqlite::params![session_id, cursor as i64],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
    });
    if !more {
        next_after = None;
    }
    Ok(Page { items, next_after })
}
