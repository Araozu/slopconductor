//! Transactional transcript branches and suffix removal. Execution records are
//! retained as audit facts; inherited turns never become executable attempts.
use super::*;
use slop_protocol::chat::{
    DeleteMessageRequest, EditMessageRequest, ForkSessionRequest, SessionFork,
};

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE sessions ADD COLUMN fork TEXT;
         ALTER TABLE turns ADD COLUMN history_only INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE messages ADD COLUMN context_excluded INTEGER NOT NULL DEFAULT 0;
         CREATE INDEX messages_request ON messages(request_id);",
    )
    .map_err(|_| StoreError::Database)
}

fn target(c: &Connection, session: &str, message: &str) -> StoreResult<(i64, String)> {
    c.query_row(
        "SELECT ordinal,role FROM messages WHERE id=?1 AND session_id=?2 AND deleted=0
         AND NOT EXISTS(SELECT 1 FROM model_requests r WHERE r.id=messages.request_id AND json_extract(r.context,'$.compaction')=1)",
        rusqlite::params![message, session],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(|_| StoreError::Database)?
    .ok_or(StoreError::NotFound)
}

fn check_revision(session: &SessionResponse, expected: Option<u64>) -> StoreResult<()> {
    if expected.is_some_and(|v| v != session.revision) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

/// A cut may leave only some results of an assistant's tool proposal. Keep its
/// public records but exclude that entire exchange from future inference.
fn exclude_unpaired(tx: &Transaction<'_>, session: &str) -> StoreResult<()> {
    tx.execute(
        "UPDATE messages SET context_excluded=1 WHERE session_id=?1 AND request_id IN (
            SELECT m.request_id FROM messages m,json_each(m.blocks) call
            WHERE m.session_id=?1 AND m.deleted=0 AND json_extract(call.value,'$.type')='tool_call'
            AND NOT EXISTS (
                SELECT 1 FROM messages result,json_each(result.blocks) block
                WHERE result.session_id=?1 AND result.deleted=0 AND result.request_id=m.request_id
                AND json_extract(block.value,'$.type')='tool_result'
                AND json_extract(block.value,'$.call_id')=json_extract(call.value,'$.call_id')))",
        [session],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

fn update_prompt(tx: &Transaction<'_>, session: &str) -> StoreResult<()> {
    tx.execute(
        "UPDATE sessions SET spec=json_set(spec,'$.prompt',COALESCE((SELECT text FROM messages WHERE session_id=?1 AND deleted=0 AND role='user' ORDER BY ordinal DESC LIMIT 1),'')),
         requested_spec=json_set(requested_spec,'$.prompt',COALESCE((SELECT text FROM messages WHERE session_id=?1 AND deleted=0 AND role='user' ORDER BY ordinal DESC LIMIT 1),'')) WHERE id=?1",
        [session],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

fn copy_prefix(
    tx: &Transaction<'_>,
    source: &str,
    destination: &str,
    through: i64,
    selected: &str,
) -> StoreResult<Option<String>> {
    let mut turns = tx
        .prepare("SELECT id,ordinal FROM turns t WHERE session_id=?1 AND EXISTS(SELECT 1 FROM messages m WHERE m.turn_id=t.id AND m.deleted=0 AND m.ordinal<=?2) ORDER BY ordinal")
        .map_err(|_| StoreError::Database)?;
    let mut rows = turns
        .query(rusqlite::params![source, through])
        .map_err(|_| StoreError::Database)?;
    let mut selected_copy = None;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        let original: String = row.get(0).map_err(|_| StoreError::Database)?;
        let ordinal: i64 = row.get(1).map_err(|_| StoreError::Database)?;
        let turn = opaque_id()?;
        let user = opaque_id()?;
        tx.execute(
            "INSERT INTO turns(id,session_id,ordinal,user_message_id,status,requested_model,resolved_model,settings,requested_settings,history_only)
             SELECT ?1,?2,ordinal,?3,CASE WHEN status IN('queued','running','paused','awaiting_children') THEN 'interrupted' ELSE status END,requested_model,resolved_model,settings,requested_settings,1 FROM turns WHERE id=?4",
            rusqlite::params![turn, destination, user, original],
        )
        .map_err(|_| StoreError::Database)?;
        // SQL copies each row directly without retaining full histories in RAM.
        // Request IDs refer to immutable original inference records, not new work.
        tx.execute(
            "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,blocks,continuation,request_id,context_excluded,provider,model)
             SELECT CASE WHEN id=(SELECT user_message_id FROM turns WHERE id=?1) THEN ?2 ELSE lower(hex(randomblob(32))) END,
             ?3,?4,ordinal,role,text,CASE WHEN status IN('pending','checkpoint') THEN 'interrupted' ELSE status END,
             blocks,CASE WHEN status='completed' THEN continuation ELSE NULL END,request_id,context_excluded,provider,model
             FROM messages WHERE turn_id=?1 AND deleted=0 AND ordinal<=?5
             AND NOT EXISTS(SELECT 1 FROM model_requests r WHERE r.id=messages.request_id AND json_extract(r.context,'$.compaction')=1)",
            rusqlite::params![original, user, destination, turn, through],
        )
        .map_err(|_| StoreError::Database)?;
        tx.execute(
            "UPDATE turns SET assistant_message_id=(SELECT id FROM messages WHERE turn_id=?1 AND role='assistant' ORDER BY ordinal DESC LIMIT 1) WHERE id=?1",
            [&turn],
        )
        .map_err(|_| StoreError::Database)?;
        if through / 1024 == ordinal {
            selected_copy = tx
                .query_row(
                    "SELECT copy.id FROM messages copy JOIN messages original ON original.ordinal=copy.ordinal WHERE copy.session_id=?1 AND original.id=?2",
                    rusqlite::params![destination, selected],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| StoreError::Database)?;
        }
    }
    exclude_unpaired(tx, destination)?;
    update_prompt(tx, destination)?;
    Ok(selected_copy)
}

fn branch(
    tx: &Transaction<'_>,
    source: &SessionResponse,
    message: &str,
    through: i64,
    title: Option<String>,
    edited: bool,
    data_dir: PathBuf,
) -> StoreResult<(String, Option<String>)> {
    // Older ordinary sessions migrated to schema 9 can have a managed
    // workspace without a project selection in their new SessionSpec.
    let selection = if let Some(selection) = &source.spec.project {
        Some(selection.clone())
    } else if let Some(id) = &source.managed_workspace_id {
        let workspace = projects::workspace(tx, id)?;
        Some(slop_protocol::projects::ProjectWorkspaceRequest {
            project_id: workspace.project_id,
            base_ref: Some(workspace.base_commit),
            allowed_tools: source
                .execution
                .as_ref()
                .ok_or(StoreError::Database)?
                .allowed_tools
                .clone(),
        })
    } else {
        None
    };
    let resolved = selection
        .as_ref()
        .map(|selection| -> StoreResult<projects::ResolvedWorkspace> {
            let project = projects::project(tx, &selection.project_id)?;
            let base_commit = if project.git_common_dir.is_empty() {
                String::new()
            } else {
                selection.base_ref.clone().ok_or(StoreError::Conflict)?
            };
            Ok(projects::ResolvedWorkspace {
                project,
                base_commit,
                data_dir,
            })
        })
        .transpose()?;
    let receipt = create_session_tx(
        tx,
        CreateSessionRequest {
            command_id: opaque_id()?,
            title: title.or_else(|| source.title.clone()),
            provider: source.provider.clone(),
            model: source.model.clone(),
            settings: Some(source.settings.clone()),
            execution: if selection.is_none() {
                source.execution.clone()
            } else {
                None
            },
            project: selection,
            budget: source.spec.budget.clone(),
            orchestration: source.spec.orchestration.clone(),
            ..Default::default()
        },
        source.settings.max_output_tokens,
        resolved,
    )?;
    let id = receipt.session_id;
    tx.execute(
        "UPDATE sessions SET fork=?1 WHERE id=?2",
        rusqlite::params![
            canonical(&SessionFork {
                session_id: source.id.clone(),
                message_id: message.to_owned(),
                edited,
            })?,
            id
        ],
    )
    .map_err(|_| StoreError::Database)?;
    let copied = copy_prefix(tx, &source.id, &id, through, message)?;
    Ok((id, copied))
}

impl StoreClient {
    pub async fn fork_session(
        &self,
        session: &str,
        request: ForkSessionRequest,
    ) -> StoreResult<CommandReceipt> {
        let session = session.to_owned();
        let data_dir = self.shared.data_dir.clone();
        self.submit(move |c| {
            if !valid_command_id(&request.command_id) {
                return Err(StoreError::Invalid);
            }
            let scope = format!("session:{session}:fork");
            let payload = canonical(&request)?;
            let tx = c
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? {
                return Ok(receipt);
            }
            let source = get_session(&tx, &session)?;
            check_revision(&source, request.expected_revision)?;
            let (through, _) = target(&tx, &session, &request.message_id)?;
            let (id, copied) = branch(
                &tx,
                &source,
                &request.message_id,
                through,
                request.title,
                false,
                data_dir,
            )?;
            let (revision, sequence) =
                append_event(&tx, &id, "session_forked", None, copied.as_deref())?;
            let receipt = make_receipt(&request.command_id, &id, None, copied, revision, sequence);
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        })
        .await
    }

    pub async fn edit_message(
        &self,
        session: &str,
        message: &str,
        request: EditMessageRequest,
        effective_settings: Option<slop_protocol::execution::GenerationSettings>,
    ) -> StoreResult<CommandReceipt> {
        let (session, message) = (session.to_owned(), message.to_owned());
        let data_dir = self.shared.data_dir.clone();
        self.submit(move |c| {
            if !valid_command_id(&request.command_id) || request.text.trim().is_empty() || request.text.len()>MAX_MESSAGE_BYTES || request.text.contains('\0') { return Err(StoreError::Invalid); }
            let scope = format!("session:{session}:message:{message}:edit");
            let payload = canonical(&request)?;
            let tx = c.unchecked_transaction().map_err(|_| StoreError::Database)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? { return Ok(receipt); }
            let source = get_session(&tx, &session)?;
            check_revision(&source, request.expected_revision)?;
            let (ordinal, role) = target(&tx, &session, &message)?;
            if !matches!(role.as_str(), "user" | "assistant") { return Err(StoreError::Invalid); }
            let through = if role=="user" { ordinal-1 } else { ordinal };
            let (id, copied) = branch(&tx, &source, &message, through, None, true, data_dir)?;
            let (turn_id, message_id) = if role=="user" {
                let accepted = send_message_tx(&tx, &id, SendMessageRequest {
                    command_id: opaque_id()?, text: request.text, ..Default::default()
                }, effective_settings, None)?;
                (accepted.turn_id, accepted.message_id)
            } else {
                let copied = copied.ok_or(StoreError::Database)?;
                // Edited assistant text is authored locally: discard original
                // provider continuation and tool proposals rather than replaying them.
                tx.execute("UPDATE messages SET text=?1,blocks='[]',continuation=NULL,request_id=NULL,status='completed',context_excluded=0 WHERE id=?2", rusqlite::params![request.text, copied]).map_err(|_| StoreError::Database)?;
                (None, Some(copied))
            };
            let (revision, sequence) = append_event(&tx, &id, "message_edited", turn_id.as_deref(), message_id.as_deref())?;
            let receipt = make_receipt(&request.command_id, &id, turn_id, message_id, revision, sequence);
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        }).await
    }

    pub async fn delete_message(
        &self,
        session: &str,
        message: &str,
        request: DeleteMessageRequest,
    ) -> StoreResult<CommandReceipt> {
        let (session, message) = (session.to_owned(), message.to_owned());
        self.submit(move |c| {
            if !valid_command_id(&request.command_id) { return Err(StoreError::Invalid); }
            let scope = format!("session:{session}:message:{message}:delete");
            let payload = canonical(&request)?;
            let tx = c.unchecked_transaction().map_err(|_| StoreError::Database)?;
            if let Some(receipt) = prior_receipt(&tx, &request.command_id, &scope, &payload)? { return Ok(receipt); }
            let source = get_session(&tx, &session)?;
            check_revision(&source, request.expected_revision)?;
            let (ordinal, _) = target(&tx, &session, &message)?;
            let busy = tx.query_row("SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=?1 AND status IN('queued','running','paused','awaiting_children'))", [&session], |r| r.get::<_,bool>(0)).map_err(|_| StoreError::Database)?;
            if busy { return Err(StoreError::HistoryBusy); }
            tx.execute("UPDATE turns SET history_only=1 WHERE session_id=?1 AND id IN(SELECT turn_id FROM messages WHERE session_id=?1 AND ordinal>=?2)", rusqlite::params![session, ordinal]).map_err(|_| StoreError::Database)?;
            tx.execute("UPDATE messages SET deleted=1 WHERE session_id=?1 AND ordinal>=?2", rusqlite::params![session, ordinal]).map_err(|_| StoreError::Database)?;
            // A summary may contain the removed suffix; rebuild from history.
            tx.execute("DELETE FROM session_context WHERE session_id=?1", [&session]).map_err(|_| StoreError::Database)?;
            exclude_unpaired(&tx, &session)?;
            update_prompt(&tx, &session)?;
            let (revision, sequence) = append_event(&tx, &session, "messages_deleted", None, Some(&message))?;
            let receipt = make_receipt(&request.command_id, &session, None, Some(message), revision, sequence);
            commit_command(&tx, &request.command_id, &scope, &payload, &receipt)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(receipt)
        }).await
    }
}
