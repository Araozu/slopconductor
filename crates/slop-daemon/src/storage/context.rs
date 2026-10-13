use super::*;
use slop_runtime::{agent::context::ContextCheckpoint, providers::inference::InferenceMessage};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE model_requests ADD COLUMN context TEXT NOT NULL DEFAULT '{}';
        CREATE TABLE session_context (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id),
            through_ordinal INTEGER NOT NULL,
            messages TEXT NOT NULL,
            request_id TEXT NOT NULL REFERENCES model_requests(id));",
    )
    .map_err(|_| StoreError::Database)
}

pub(super) fn load(c: &Connection, session: &str) -> StoreResult<(i64, Vec<InferenceMessage>)> {
    c.query_row(
        "SELECT through_ordinal,messages FROM session_context WHERE session_id=?1",
        [session],
        |r| Ok((r.get(0)?, execution::json_column(r, 1)?)),
    )
    .optional()
    .map_err(|_| StoreError::Database)
    .map(|v| v.unwrap_or_default())
}

pub(super) fn save(
    tx: &Transaction<'_>,
    session: &str,
    turn: &str,
    request: &str,
    checkpoint: ContextCheckpoint,
) -> StoreResult<()> {
    let encoded = canonical(&checkpoint.messages)?;
    if encoded.len() > MAX_CONTEXT_BYTES || checkpoint.messages.len() > MAX_CONTEXT_MESSAGES {
        return Err(StoreError::Limit);
    }
    let last: i64 = tx.query_row("SELECT COALESCE(MAX(m.ordinal),0) FROM messages m JOIN turns t ON t.id=m.turn_id WHERE m.session_id=?1 AND m.deleted=0 AND m.context_excluded=0 AND t.ordinal<=(SELECT ordinal FROM turns WHERE id=?2) AND m.status IN('completed','incomplete') AND NOT EXISTS(SELECT 1 FROM model_requests r WHERE r.id=m.request_id AND json_extract(r.context,'$.compaction')=1)",
        rusqlite::params![session,turn], |r| r.get(0)).map_err(|_| StoreError::Database)?;
    let through = checkpoint.through_ordinal.unwrap_or(last);
    let previous = load(tx, session)?.0;
    if through < previous || through > last || checkpoint.messages.is_empty() {
        return Err(StoreError::Invalid);
    }
    tx.execute("INSERT INTO session_context(session_id,through_ordinal,messages,request_id) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET through_ordinal=excluded.through_ordinal,messages=excluded.messages,request_id=excluded.request_id",
        rusqlite::params![session,through,encoded,request]).map_err(|_| StoreError::Database)?;
    append_event(tx, session, "context_compacted", Some(turn), None)?;
    Ok(())
}
