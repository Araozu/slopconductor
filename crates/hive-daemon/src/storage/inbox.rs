use super::*;
use hive_protocol::chat::{
    ConversationDraft, SaveConversationDraftRequest, UpdateSessionInboxRequest,
};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE sessions ADD COLUMN inbox TEXT;
         CREATE TABLE conversation_drafts (
             created_order INTEGER PRIMARY KEY AUTOINCREMENT,
             id TEXT NOT NULL UNIQUE,
             draft TEXT NOT NULL
         );",
    )
    .map_err(|_| StoreError::Database)
}

impl StoreClient {
    pub async fn update_session_inbox(
        &self,
        id: &str,
        request: UpdateSessionInboxRequest,
    ) -> StoreResult<CommandReceipt> {
        let id = id.to_owned();
        self.submit(move |connection| update(connection, &id, request))
            .await
    }

    pub async fn drafts(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<ConversationDraft>> {
        let limit = page_limit(limit)?;
        self.submit(move |connection| {
            let after = after.unwrap_or(0).min(i64::MAX as u64) as i64;
            let mut statement = connection.prepare("SELECT created_order,draft FROM conversation_drafts WHERE created_order>?1 ORDER BY created_order LIMIT ?2").map_err(|_| StoreError::Database)?;
            let mut rows = statement.query(rusqlite::params![after, (limit + 1) as i64]).map_err(|_| StoreError::Database)?;
            let mut items = Vec::new();
            let mut cursor = None;
            let mut next_after = None;
            let mut bytes = 0;
            while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
                let encoded: String = row.get(1).map_err(|_| StoreError::Database)?;
                if items.len() == limit || !items.is_empty() && bytes + encoded.len() > 4 * 1024 * 1024 {
                    next_after = cursor;
                    break;
                }
                bytes += encoded.len();
                cursor = Some(row.get::<_, i64>(0).map_err(|_| StoreError::Database)? as u64);
                items.push(serde_json::from_str(&encoded).map_err(|_| StoreError::Database)?);
            }
            Ok(Page { items, next_after })
        }).await
    }

    pub async fn save_draft(
        &self,
        id: &str,
        request: SaveConversationDraftRequest,
    ) -> StoreResult<ConversationDraft> {
        if id != request.draft.id
            || !valid_command_id(id)
            || request.draft.title.len() > 256
            || request.draft.title.chars().any(char::is_control)
            || request.draft.text.len() > MAX_MESSAGE_BYTES
            || request.draft.model.len() > 384
            || request.draft.workspace.as_ref().is_some_and(|w| {
                w.workspace_id.len() > 128
                    || w.allowed_tools.len() > 4
                    || w.allowed_tools
                        .iter()
                        .any(|t| !["read", "write", "edit", "bash"].contains(&t.as_str()))
            })
            || canonical(&request.draft)?.len() > 128 * 1024
        {
            return Err(StoreError::Invalid);
        }
        self.submit(move |connection| {
            let tx = connection.unchecked_transaction().map_err(|_| StoreError::Database)?;
            let draft = request.draft;
            let conflict = if request.only_if_absent { "DO NOTHING" } else { "DO UPDATE SET draft=excluded.draft" };
            tx.execute(&format!("INSERT INTO conversation_drafts(id,draft) VALUES(?1,?2) ON CONFLICT(id) {conflict}"), rusqlite::params![draft.id, canonical(&draft)?]).map_err(|_| StoreError::Database)?;
            let saved = tx.query_row("SELECT draft FROM conversation_drafts WHERE id=?1", [&draft.id], |row| execution::json_column(row, 0)).map_err(|_| StoreError::Database)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(saved)
        }).await
    }

    pub async fn remove_draft(&self, id: &str) -> StoreResult<()> {
        let id = id.to_owned();
        self.submit(move |connection| {
            connection
                .execute("DELETE FROM conversation_drafts WHERE id=?1", [id])
                .map_err(|_| StoreError::Database)?;
            Ok(())
        })
        .await
    }
}

fn update(
    connection: &Connection,
    id: &str,
    request: UpdateSessionInboxRequest,
) -> StoreResult<CommandReceipt> {
    if !valid_command_id(&request.command_id) {
        return Err(StoreError::Invalid);
    }
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let scope = format!("session_inbox:{id}");
    let payload = canonical(&request)?;
    if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
        return Ok(receipt);
    }
    let session = get_session(&tx, id)?;
    let mut inbox = session.inbox.clone().unwrap_or_default();
    let mut revision = session.revision;
    let mut sequence = session.last_event_sequence;
    if !(request.only_if_unset && session.inbox.is_some()) {
        if request.read_sequence.is_some_and(|read| read > sequence) {
            return Err(StoreError::Invalid);
        }
        if let Some(value) = request.starred {
            inbox.starred = value;
        }
        if let Some(value) = request.archived {
            inbox.archived = value;
        }
        if let Some(value) = request.trashed {
            inbox.trashed = value;
        }
        if let Some(value) = request.read_sequence {
            inbox.read_sequence = inbox.read_sequence.max(value);
        }
        if session.inbox.as_ref() != Some(&inbox) {
            let was_read = inbox.read_sequence == sequence;
            (revision, sequence) = append_event(&tx, id, "session_inbox_updated", None, None)?;
            // An organization change must not make an already-read thread unread.
            if was_read {
                inbox.read_sequence = sequence;
            }
            tx.execute(
                "UPDATE sessions SET inbox=?1 WHERE id=?2",
                rusqlite::params![canonical(&inbox)?, id],
            )
            .map_err(|_| StoreError::Database)?;
        }
    }
    let receipt = make_receipt(&request.command_id, id, None, None, revision, sequence);
    commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(receipt)
}
