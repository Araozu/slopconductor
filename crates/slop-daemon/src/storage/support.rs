use super::*;

pub(super) fn valid_command_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_COMMAND_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

pub(super) fn page_limit(limit: usize) -> StoreResult<usize> {
    let limit = if limit == 0 { DEFAULT_PAGE_SIZE } else { limit };
    if limit > MAX_PAGE_SIZE {
        Err(StoreError::Invalid)
    } else {
        Ok(limit)
    }
}

pub(super) fn opaque_id() -> StoreResult<String> {
    random_node_id()
}

pub(super) fn canonical<T: serde::Serialize>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(|_| StoreError::Invalid)
}

pub(super) fn prior_receipt(
    connection: &Connection,
    command_id: &str,
    scope: &str,
    payload: &str,
) -> StoreResult<Option<CommandReceipt>> {
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

pub(super) fn commit_command(
    tx: &Transaction<'_>,
    command_id: &str,
    scope: &str,
    payload: &str,
    receipt: &CommandReceipt,
) -> StoreResult<()> {
    let receipt = canonical(receipt)?;
    tx.execute(
        "INSERT INTO commands(command_id,scope,payload,receipt) VALUES(?1,?2,?3,?4)",
        rusqlite::params![command_id, scope, payload, receipt],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

pub(super) fn append_event(
    tx: &Transaction<'_>,
    session_id: &str,
    kind: &str,
    turn_id: Option<&str>,
    message_id: Option<&str>,
) -> StoreResult<(u64, u64)> {
    tx.execute("UPDATE sessions SET revision=revision+1,last_event_sequence=last_event_sequence+1 WHERE id=?1",[session_id])
        .map_err(|_| StoreError::Database)?;
    if tx.changes() != 1 {
        return Err(StoreError::NotFound);
    }
    let (revision, sequence): (i64, i64) = tx
        .query_row(
            "SELECT revision,last_event_sequence FROM sessions WHERE id=?1",
            [session_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| StoreError::Database)?;
    tx.execute("INSERT INTO session_events(session_id,sequence,kind,turn_id,message_id,revision) VALUES(?1,?2,?3,?4,?5,?6)",
        rusqlite::params![session_id,sequence,kind,turn_id,message_id,revision]).map_err(|_|StoreError::Database)?;
    orchestration::session_event(tx, session_id, kind, turn_id, sequence as u64)?;
    Ok((revision as u64, sequence as u64))
}

pub(super) fn make_receipt(
    command_id: &str,
    session_id: &str,
    turn_id: Option<String>,
    message_id: Option<String>,
    revision: u64,
    event_sequence: u64,
) -> CommandReceipt {
    CommandReceipt {
        command_id: command_id.to_owned(),
        session_id: session_id.to_owned(),
        turn_id,
        message_id,
        revision,
        event_sequence,
    }
}
