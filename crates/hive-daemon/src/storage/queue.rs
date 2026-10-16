//! The owner, rather than a connected client, delivers the session's FIFO.
use super::*;
use hive_protocol::chat::{
    InterruptSessionRequest, QueuedMessageResponse, UpdateQueuedMessageRequest,
};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS message_queue (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL REFERENCES sessions(id),
            turn_id TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL DEFAULT 'pending',
            delivery TEXT NOT NULL CHECK(delivery IN ('after-turn','after-finish')),
            request TEXT NOT NULL,
            settings TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS message_queue_session ON message_queue(session_id,sequence);",
    )
    .map_err(|_| StoreError::Database)
}

fn active(connection: &Connection, session: &str) -> StoreResult<Option<String>> {
    connection.query_row(
        "SELECT id FROM turns WHERE session_id=?1 AND status IN('queued','running','paused','awaiting_children') ORDER BY ordinal LIMIT 1",
        [session], |row| row.get(0),
    ).optional().map_err(|_| StoreError::Database)
}

pub(super) fn virtual_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    let (session, message, request, settings, status): (
        String,
        String,
        SendMessageRequest,
        hive_protocol::execution::GenerationSettings,
        String,
    ) = connection
        .query_row(
            "SELECT session_id,id,request,settings,status FROM message_queue WHERE turn_id=?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    execution::json_column(row, 2)?,
                    execution::json_column(row, 3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    let parent = get_session(connection, &session)?;
    Ok(TurnResponse {
        id: id.into(),
        session_id: session,
        user_message_id: message,
        assistant_message_id: None,
        status: match status.as_str() {
            "pending" => "queued",
            "cancelled" => "cancelled",
            _ => "completed",
        }
        .into(),
        requested_model: request
            .model
            .unwrap_or_else(|| format!("{}/{}", parent.provider, parent.model)),
        resolved_model: None,
        usage: None,
        error_code: None,
        error_message: None,
        settings,
        model_request_ids: Vec::new(),
        tool_invocation_ids: Vec::new(),
    })
}

pub(super) fn cancel_pending_tx(
    tx: &Transaction<'_>,
    id: &str,
    request: &CancelTurnRequest,
) -> StoreResult<Option<CommandReceipt>> {
    let entry: Option<(String, String)> = tx
        .query_row(
            "SELECT session_id,id FROM message_queue WHERE turn_id=?1 AND status='pending'",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    let Some((session, message)) = entry else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE message_queue SET status='cancelled' WHERE turn_id=?1",
        [id],
    )
    .map_err(|_| StoreError::Database)?;
    let (revision, sequence) = append_event(
        tx,
        &session,
        "queued_message_cancelled",
        Some(id),
        Some(&message),
    )?;
    let receipt = make_receipt(
        &request.command_id,
        &session,
        Some(id.into()),
        Some(message),
        revision,
        sequence,
    );
    commit_command(
        tx,
        &request.command_id,
        &format!("turn:{id}:cancel"),
        &canonical(request)?,
        &receipt,
    )?;
    Ok(Some(receipt))
}

/// Return None when idle, allowing ordinary immediate admission.
pub(super) fn accept_tx(
    tx: &Transaction<'_>,
    session: &SessionResponse,
    request: &SendMessageRequest,
    settings: Option<&hive_protocol::execution::GenerationSettings>,
) -> StoreResult<Option<CommandReceipt>> {
    let Some(turn) = active(tx, &session.id)? else {
        return Ok(None);
    };
    let delivery = request.delivery.unwrap_or_default();
    let (local, global): (i64, i64) = tx.query_row(
        "SELECT (SELECT count(*) FROM message_queue WHERE session_id=?1 AND status='pending'), (SELECT count(*) FROM message_queue WHERE status='pending')",
        [&session.id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).map_err(|_| StoreError::Database)?;
    if local >= MAX_QUEUE_PER_SESSION || global >= MAX_QUEUE_GLOBAL {
        return Err(StoreError::Limit);
    }
    let model = request
        .model
        .clone()
        .unwrap_or_else(|| format!("{}/{}", session.provider, session.model));
    let settings = match settings {
        Some(settings) => {
            execution::validate_settings(&model, settings)?;
            settings.clone()
        }
        None => execution::turn_settings(session, request, &model)?,
    };
    let id = opaque_id()?;
    let reserved_turn = opaque_id()?;
    tx.execute(
        "INSERT INTO message_queue(id,session_id,delivery,request,settings,turn_id) VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![
            id,
            session.id,
            if delivery == DeliveryMode::AfterTurn {
                "after-turn"
            } else {
                "after-finish"
            },
            canonical(request)?,
            canonical(&settings)?,
            reserved_turn
        ],
    )
    .map_err(|_| StoreError::Database)?;
    // Older clients may have left a durable pause. Sending never requires an
    // unpause command: the existing run is automatically admitted again.
    tx.execute(
        "UPDATE turns SET status=CASE WHEN status='paused' THEN 'queued' ELSE status END,pause_requested=0 WHERE id=?1",
        [&turn],
    )
    .map_err(|_| StoreError::Database)?;
    let (revision, sequence) =
        append_event(tx, &session.id, "message_queued", Some(&turn), Some(&id))?;
    let receipt = make_receipt(
        &request.command_id,
        &session.id,
        Some(if delivery == DeliveryMode::AfterFinish {
            reserved_turn
        } else {
            turn
        }),
        Some(id),
        revision,
        sequence,
    );
    commit_command(
        tx,
        &request.command_id,
        &format!("session:{}:message", session.id),
        &canonical(request)?,
        &receipt,
    )?;
    Ok(Some(receipt))
}

struct Entry {
    id: String,
    turn_id: String,
    request: SendMessageRequest,
    settings: hive_protocol::execution::GenerationSettings,
}

fn entries(connection: &Connection, session: &str) -> StoreResult<Vec<Entry>> {
    let mut statement = connection.prepare("SELECT id,request,settings,turn_id FROM message_queue WHERE session_id=?1 AND status='pending' ORDER BY sequence LIMIT ?2")
        .map_err(|_| StoreError::Database)?;
    statement
        .query_map(rusqlite::params![session, MAX_QUEUE_PER_SESSION], |row| {
            Ok(Entry {
                id: row.get(0)?,
                turn_id: row.get(3)?,
                request: execution::json_column(row, 1)?,
                settings: execution::json_column(row, 2)?,
            })
        })
        .map_err(|_| StoreError::Database)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StoreError::Database)
}

/// A new run must make its first model request before consuming another batch.
/// This also ensures interrupting a finish message sends only that one message.
pub(super) fn boundary_pending(connection: &Connection, turn: &str) -> StoreResult<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM model_requests WHERE turn_id=t.id AND status='completed')
         AND NOT EXISTS(SELECT 1 FROM tool_invocations WHERE turn_id=t.id AND status IN('intent','running'))
         AND (SELECT delivery FROM message_queue WHERE session_id=t.session_id AND status='pending' ORDER BY sequence LIMIT 1)='after-turn'
         FROM turns t WHERE id=?1", [turn], |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
    ).map_err(|_| StoreError::Database)
}

fn append_message(
    tx: &Transaction<'_>,
    session: &str,
    turn: &str,
    entry: &Entry,
) -> StoreResult<()> {
    let model = messages::turn_model(tx, turn)?;
    let ordinal = execution::next_message_ordinal(tx, turn)?;
    tx.execute(
        "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,provider,model) VALUES(?1,?2,?3,?4,'user',?5,'completed',?6,?7)",
        rusqlite::params![entry.id, session, turn, ordinal, entry.request.text, model.provider().as_str(), model.model()],
    ).map_err(|_| StoreError::Database)?;
    sent(tx, session, turn, &entry.id)
}

fn sent(tx: &Transaction<'_>, session: &str, turn: &str, id: &str) -> StoreResult<()> {
    tx.execute("UPDATE message_queue SET status='sent' WHERE id=?1", [id])
        .map_err(|_| StoreError::Database)?;
    append_event(tx, session, "queued_message_sent", Some(turn), Some(id))?;
    Ok(())
}

pub(super) fn apply_boundary_tx(tx: &Transaction<'_>, turn: &str) -> StoreResult<usize> {
    if !boundary_pending(tx, turn)? {
        return Ok(0);
    }
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |row| {
            row.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    let mut count = 0;
    for entry in entries(tx, &session)? {
        if entry.request.delivery.unwrap_or_default() != DeliveryMode::AfterTurn {
            break;
        }
        append_message(tx, &session, turn, &entry)?;
        count += 1;
    }
    Ok(count)
}

/// At finish/cancellation, admit the leading after-turn batch OR one finish
/// message. Later messages remain in the FIFO until their own boundary.
pub(super) fn release_tx(tx: &Transaction<'_>, session: &str) -> StoreResult<()> {
    if active(tx, session)?.is_some() {
        return Ok(());
    }
    let mut entries = entries(tx, session)?.into_iter();
    let Some(mut first) = entries.next() else {
        return Ok(());
    };
    let batch = first.request.delivery.unwrap_or_default() == DeliveryMode::AfterTurn;
    first.request.command_id = format!("dispatch-{}", first.id);
    first.request.expected_revision = None;
    first.request.delivery = Some(DeliveryMode::AfterFinish);
    let receipt = messages::send_direct_tx(
        tx,
        session,
        first.request,
        Some(first.settings),
        None,
        Some((&first.id, &first.turn_id)),
    )?;
    let turn = receipt.turn_id.ok_or(StoreError::Database)?;
    sent(tx, session, &turn, &first.id)?;
    if batch {
        for entry in entries {
            if entry.request.delivery.unwrap_or_default() != DeliveryMode::AfterTurn {
                break;
            }
            append_message(tx, session, &turn, &entry)?;
        }
    }
    Ok(())
}

impl StoreClient {
    /// Mutations serialize with dispatch; an already dispatched entry is never
    /// edited or used as a cancellation target for the active turn.
    pub async fn update_queued_message(
        &self,
        session: &str,
        message: &str,
        request: UpdateQueuedMessageRequest,
    ) -> StoreResult<CommandReceipt> {
        let session = session.to_owned();
        let message = message.to_owned();
        self.submit(move |connection| {
            if !valid_command_id(&request.command_id)
                || (request.text.is_none() && request.delivery.is_none())
                || request.text.as_ref().is_some_and(|text| {
                    text.trim().is_empty() || text.len() > MAX_MESSAGE_BYTES || text.contains('\0')
                })
                || request.delivery.is_some_and(|mode| {
                    !matches!(mode, DeliveryMode::AfterTurn | DeliveryMode::AfterFinish)
                })
            {
                return Err(StoreError::Invalid);
            }
            let tx = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope = format!("session:{session}:queue:{message}:update");
            let payload = canonical(&request)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let (turn, mut queued) = pending_entry(&tx, &session, &message)?;
            if let Some(text) = request.text {
                queued.text = text;
            }
            if let Some(delivery) = request.delivery {
                queued.delivery = Some(delivery);
            }
            let delivery = if queued.delivery.unwrap_or_default() == DeliveryMode::AfterTurn {
                "after-turn"
            } else {
                "after-finish"
            };
            tx.execute(
                "UPDATE message_queue SET request=?1,delivery=?2 WHERE id=?3",
                rusqlite::params![canonical(&queued)?, delivery, message],
            )
            .map_err(|_| StoreError::Database)?;
            let (revision, sequence) = append_event(
                &tx,
                &session,
                "queued_message_updated",
                Some(&turn),
                Some(&message),
            )?;
            let receipt = make_receipt(
                &request.command_id,
                &session,
                Some(turn),
                Some(message),
                revision,
                sequence,
            );
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn cancel_queued_message(
        &self,
        session: &str,
        message: &str,
        request: CancelTurnRequest,
    ) -> StoreResult<CommandReceipt> {
        let session = session.to_owned();
        let message = message.to_owned();
        self.submit(move |connection| {
            if !valid_command_id(&request.command_id) {
                return Err(StoreError::Invalid);
            }
            let tx = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope = format!("session:{session}:queue:{message}:cancel");
            let payload = canonical(&request)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let (turn, _) = pending_entry(&tx, &session, &message)?;
            tx.execute(
                "UPDATE message_queue SET status='cancelled' WHERE id=?1",
                [&message],
            )
            .map_err(|_| StoreError::Database)?;
            let (revision, sequence) = append_event(
                &tx,
                &session,
                "queued_message_cancelled",
                Some(&turn),
                Some(&message),
            )?;
            let receipt = make_receipt(
                &request.command_id,
                &session,
                Some(turn),
                Some(message),
                revision,
                sequence,
            );
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn queued_messages(&self, session: &str) -> StoreResult<Vec<QueuedMessageResponse>> {
        let session = session.to_owned();
        self.submit(move |connection| {
            get_session(connection, &session)?;
            entries(connection, &session)?
                .into_iter()
                .map(|entry| {
                    Ok(QueuedMessageResponse {
                        id: entry.id,
                        text: entry.request.text,
                        delivery: entry.request.delivery.unwrap_or_default(),
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn interrupt_session(
        &self,
        session: &str,
        request: InterruptSessionRequest,
    ) -> StoreResult<CommandReceipt> {
        self.interrupt_session_preflighted(session, request, Vec::new())
            .await
    }

    pub async fn interrupt_session_preflighted(
        &self,
        session: &str,
        request: InterruptSessionRequest,
        settings: Vec<Option<hive_protocol::execution::GenerationSettings>>,
    ) -> StoreResult<CommandReceipt> {
        let session = session.to_owned();
        self.submit(move |connection| {
            if !valid_command_id(&request.command_id) || request.messages.len() > 33 {
                return Err(StoreError::Invalid);
            }
            let tx = connection
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope = format!("session:{session}:interrupt");
            let payload = canonical(&request)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            get_session(&tx, &session)?;
            // Queued input and cancellation commit together. A concurrent POST
            // with the same message command ID receives its original receipt.
            // Rejected prompts remain the sender's responsibility; they must
            // never prevent the stop command from taking effect.
            for (index, message) in request.messages.iter().enumerate() {
                let effective = if settings.is_empty() {
                    None
                } else {
                    let Some(Some(settings)) = settings.get(index) else {
                        continue;
                    };
                    Some(settings.clone())
                };
                tx.execute_batch("SAVEPOINT interrupt_message")
                    .map_err(|_| StoreError::Database)?;
                if messages::send_message_tx(&tx, &session, message.clone(), effective, None)
                    .is_err()
                {
                    tx.execute_batch("ROLLBACK TO interrupt_message")
                        .map_err(|_| StoreError::Database)?;
                }
                tx.execute_batch("RELEASE interrupt_message")
                    .map_err(|_| StoreError::Database)?;
            }
            let target = match request.turn_id {
                Some(id) => {
                    let turn = get_turn(&tx, &id)?;
                    if turn.session_id != session {
                        return Err(StoreError::Invalid);
                    }
                    if matches!(
                        turn.status.as_str(),
                        "queued" | "running" | "paused" | "awaiting_children"
                    ) {
                        Some(id)
                    } else {
                        None
                    }
                }
                None => active(&tx, &session)?,
            };
            if let Some(turn) = &target {
                cancel_turn_tx(
                    &tx,
                    turn,
                    CancelTurnRequest {
                        command_id: opaque_id()?,
                    },
                )?;
            }
            release_tx(&tx, &session)?;
            let (revision, sequence) = append_event(
                &tx,
                &session,
                "session_interrupt_requested",
                target.as_deref(),
                None,
            )?;
            let receipt = make_receipt(
                &request.command_id,
                &session,
                target,
                None,
                revision,
                sequence,
            );
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }
}

fn pending_entry(
    connection: &Connection,
    session: &str,
    message: &str,
) -> StoreResult<(String, SendMessageRequest)> {
    get_session(connection, session)?;
    let entry: Option<(String, String, SendMessageRequest)> = connection
        .query_row(
            "SELECT turn_id,status,request FROM message_queue WHERE id=?1 AND session_id=?2",
            rusqlite::params![message, session],
            |row| Ok((row.get(0)?, row.get(1)?, execution::json_column(row, 2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?;
    match entry {
        Some((turn, status, request)) if status == "pending" => Ok((turn, request)),
        Some(_) => Err(StoreError::Conflict),
        None => Err(StoreError::NotFound),
    }
}
