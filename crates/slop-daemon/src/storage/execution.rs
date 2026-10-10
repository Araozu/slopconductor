//! Transactional structured request/tool records and schema-2 migration.
use super::*;
use slop_protocol::execution::{self as wire, GenerationSettings};
use slop_runtime::{
    agent::{RequestCompletion, RequestIntent, ToolIntent},
    providers::inference::{
        self as inference, BlockContent, ContentBlock, InferenceMessage, MessageRole,
    },
    tools::{ToolOutcome, WorkspacePolicy},
};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE sessions ADD COLUMN settings TEXT NOT NULL DEFAULT '{}';
        ALTER TABLE sessions ADD COLUMN execution TEXT;
        ALTER TABLE sessions ADD COLUMN workspace_root TEXT;
        ALTER TABLE turns ADD COLUMN settings TEXT NOT NULL DEFAULT '{}';
        ALTER TABLE turns ADD COLUMN requested_settings TEXT NOT NULL DEFAULT '{}';
        ALTER TABLE messages ADD COLUMN blocks TEXT NOT NULL DEFAULT '[]';
        ALTER TABLE messages ADD COLUMN continuation TEXT;
        ALTER TABLE messages ADD COLUMN request_id TEXT;
        ALTER TABLE session_events ADD COLUMN request_id TEXT;
        ALTER TABLE session_events ADD COLUMN invocation_id TEXT;
        UPDATE sessions SET settings=json_object('max_output_tokens',max_tokens,'reasoning_effort',NULL);
        UPDATE turns SET settings=(SELECT settings FROM sessions WHERE sessions.id=turns.session_id);
        UPDATE messages SET ordinal=-ordinal;
        UPDATE messages SET ordinal=(SELECT ordinal*1024 FROM turns WHERE turns.id=messages.turn_id)+CASE WHEN role='user' THEN 0 ELSE 1 END;
        CREATE TABLE model_requests (
            id TEXT PRIMARY KEY,turn_id TEXT NOT NULL REFERENCES turns(id),message_id TEXT NOT NULL REFERENCES messages(id),ordinal INTEGER NOT NULL,
            requested_model TEXT NOT NULL,requested_settings TEXT NOT NULL,settings TEXT NOT NULL,status TEXT NOT NULL,
            resolved_model TEXT,finish_reason TEXT,usage TEXT,error_code TEXT,UNIQUE(turn_id,ordinal));
        CREATE TABLE tool_invocations (
            id TEXT PRIMARY KEY,turn_id TEXT NOT NULL REFERENCES turns(id),request_id TEXT NOT NULL REFERENCES model_requests(id),
            call_id TEXT NOT NULL UNIQUE,provider_call_id TEXT NOT NULL,result_message_id TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,arguments TEXT NOT NULL,status TEXT NOT NULL,outcome TEXT);
        CREATE INDEX tool_invocations_turn ON tool_invocations(turn_id);
        CREATE INDEX model_requests_turn ON model_requests(turn_id);
        CREATE INDEX sessions_workspace ON sessions(workspace_root);
        CREATE TABLE artifacts(id TEXT PRIMARY KEY,sha256 TEXT NOT NULL,size_bytes INTEGER NOT NULL,media_type TEXT NOT NULL);")
        .map_err(|_|StoreError::Database)
}

pub(super) fn json_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let value: String = row.get(index)?;
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}
pub(super) fn optional_json_column<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    index,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .transpose()
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> StoreResult<T> {
    serde_json::from_str(value).map_err(|_| StoreError::Database)
}

pub(super) fn session_settings(
    request: &CreateSessionRequest,
    default: Option<u32>,
) -> StoreResult<GenerationSettings> {
    let mut settings = request.settings.clone().unwrap_or_default();
    if settings
        .max_output_tokens
        .zip(request.max_tokens)
        .is_some_and(|(a, b)| a != b)
    {
        return Err(StoreError::Invalid);
    }
    settings.max_output_tokens = settings
        .max_output_tokens
        .or(request.max_tokens)
        .or(default);
    validate_settings(
        &format!("{}/{}", request.provider, request.model),
        &settings,
    )?;
    Ok(settings)
}
pub(super) fn validate_settings(model: &str, settings: &GenerationSettings) -> StoreResult<()> {
    let selected: slop_core::provider::ProviderModelRef =
        match model.parse::<slop_core::provider::ProviderModelRef>() {
            Ok(model) => model,
            Err(_) => return Err(StoreError::Invalid),
        };
    let Some(provider) = slop_runtime::providers::provider(selected.provider()) else {
        return Err(StoreError::Unsupported("model"));
    };
    if provider.wire_protocol(selected.model()).is_err() {
        return Err(StoreError::Unsupported("model"));
    }
    if settings
        .max_output_tokens
        .is_some_and(|n| n == 0 || n > 65536)
    {
        return Err(StoreError::Invalid);
    }
    let capabilities = inference::capabilities(provider, selected.model());
    if settings
        .reasoning_effort
        .as_ref()
        .is_some_and(|effort| !capabilities.reasoning_efforts.contains(effort))
    {
        return Err(StoreError::Unsupported("settings.reasoning_effort"));
    }
    Ok(())
}
pub(super) fn turn_settings(
    session: &SessionResponse,
    request: &SendMessageRequest,
    model: &str,
) -> StoreResult<GenerationSettings> {
    let mut settings = request
        .settings
        .clone()
        .unwrap_or_else(|| session.settings.clone());
    settings.max_output_tokens = settings
        .max_output_tokens
        .or(session.settings.max_output_tokens);
    validate_settings(model, &settings)?;
    Ok(settings)
}
pub(super) fn workspace(request: &CreateSessionRequest) -> StoreResult<Option<WorkspacePolicy>> {
    request
        .execution
        .as_ref()
        .map(|policy| {
            let runtime: WorkspacePolicy = serde_json::from_value(
                serde_json::to_value(policy).map_err(|_| StoreError::Invalid)?,
            )
            .map_err(|_| StoreError::Invalid)?;
            runtime.canonicalize().map_err(|_| StoreError::Invalid)
        })
        .transpose()
}

fn public_block(block: ContentBlock) -> wire::ContentBlock {
    wire::ContentBlock {
        id: block.id,
        content: match block.content {
            BlockContent::Text { text } => wire::BlockContent::Text { text },
            BlockContent::Refusal { text } => wire::BlockContent::Refusal { text },
            BlockContent::ToolCall {
                call_id,
                name,
                arguments,
                ..
            } => wire::BlockContent::ToolCall {
                call_id,
                name,
                arguments,
            },
            BlockContent::ToolResult {
                call_id,
                is_error,
                output,
                artifact_ids,
                effects_unknown,
                ..
            } => wire::BlockContent::ToolResult {
                call_id,
                is_error,
                output,
                artifact_ids,
                effects_unknown,
            },
        },
    }
}
pub(super) fn public_blocks_from_row(
    row: &rusqlite::Row<'_>,
    blocks_index: usize,
    id_index: usize,
    text_index: usize,
) -> StoreResult<Vec<wire::ContentBlock>> {
    let mut blocks: Vec<ContentBlock> =
        json_column(row, blocks_index).map_err(|_| StoreError::Database)?;
    if blocks.is_empty() {
        let id: String = row.get(id_index).map_err(|_| StoreError::Database)?;
        let text: String = row.get(text_index).map_err(|_| StoreError::Database)?;
        blocks.push(ContentBlock {
            id: format!("{id}:block:0"),
            content: BlockContent::Text { text },
        });
    }
    Ok(blocks.into_iter().map(public_block).collect())
}
pub(super) fn next_message_ordinal(tx: &Transaction<'_>, turn: &str) -> StoreResult<i64> {
    let (base,last):(i64,Option<i64>)=tx.query_row("SELECT t.ordinal*1024,(SELECT MAX(ordinal) FROM messages WHERE turn_id=t.id) FROM turns t WHERE id=?1",[turn],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|StoreError::Database)?;
    let next = last
        .unwrap_or(base)
        .checked_add(1)
        .ok_or(StoreError::Limit)?;
    if next >= base + 1024 {
        return Err(StoreError::Limit);
    }
    Ok(next)
}
fn running(tx: &Transaction<'_>, turn: &str) -> StoreResult<String> {
    let (session, status, cancel): (String, String, bool) = tx
        .query_row(
            "SELECT session_id,status,cancellation_requested FROM turns WHERE id=?1",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    if status != "running" || cancel {
        return Err(StoreError::Conflict);
    }
    Ok(session)
}
fn running_for_step(tx: &Transaction<'_>, turn: &str) -> StoreResult<Option<String>> {
    let (session, status, cancel): (String, String, bool) = tx
        .query_row(
            "SELECT session_id,status,cancellation_requested FROM turns WHERE id=?1",
            [turn],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    if status != "running" {
        return Err(StoreError::Conflict);
    }
    Ok((!cancel).then_some(session))
}
fn event(
    tx: &Transaction<'_>,
    session: &str,
    kind: &str,
    turn: &str,
    message: Option<&str>,
    request: Option<&str>,
    invocation: Option<&str>,
) -> StoreResult<()> {
    let (_, seq) = append_event(tx, session, kind, Some(turn), message)?;
    tx.execute("UPDATE session_events SET request_id=?1,invocation_id=?2 WHERE session_id=?3 AND sequence=?4",rusqlite::params![request,invocation,session,seq as i64]).map_err(|_|StoreError::Database)?;
    Ok(())
}

pub(super) fn begin_request(
    connection: &Connection,
    turn: &str,
    intent: RequestIntent,
) -> StoreResult<bool> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let Some(session) = running_for_step(&tx, turn)? else {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    };
    let yielding: bool = tx
        .query_row(
            "SELECT pause_requested!=0 OR EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='accepted') OR EXISTS(SELECT 1 FROM child_waits w WHERE w.parent_turn_id=?1 AND w.completed=0) FROM turns WHERE id=?1",
            [turn],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if yielding {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    }
    if !super::governance::reserve(&tx, turn, true)? {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    }
    let ordinal = next_message_ordinal(&tx, turn)?;
    tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,request_id) VALUES(?1,?2,?3,?4,'assistant','','pending',?5)",rusqlite::params![intent.message_id,session,turn,ordinal,intent.id]).map_err(|_|StoreError::Database)?;
    tx.execute("INSERT INTO model_requests(id,turn_id,message_id,ordinal,requested_model,requested_settings,settings,status) VALUES(?1,?2,?3,?4,?5,?6,?7,'running')",rusqlite::params![intent.id,turn,intent.message_id,intent.ordinal,intent.requested_model,canonical(&intent.requested_settings)?,canonical(&intent.settings)?]).map_err(|_|StoreError::Database)?;
    tx.execute(
        "UPDATE turns SET assistant_message_id=?1 WHERE id=?2",
        rusqlite::params![intent.message_id, turn],
    )
    .map_err(|_| StoreError::Database)?;
    event(
        &tx,
        &session,
        "model_request_started",
        turn,
        Some(&intent.message_id),
        Some(&intent.id),
        None,
    )?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(true)
}
pub(super) fn checkpoint_request(
    connection: &Connection,
    turn: &str,
    request: &str,
    message: InferenceMessage,
) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let session = running(&tx, turn)?;
    let message_id: String = tx
        .query_row(
            "SELECT message_id FROM model_requests WHERE id=?1 AND turn_id=?2 AND status='running'",
            rusqlite::params![request, turn],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::Conflict)?;
    let text = message.visible_text();
    if text.len() > MAX_CONTEXT_BYTES {
        return Err(StoreError::Limit);
    }
    tx.execute(
        "UPDATE messages SET text=?1,blocks=?2,status='checkpoint' WHERE id=?3",
        rusqlite::params![text, canonical(&message.blocks)?, message_id],
    )
    .map_err(|_| StoreError::Database)?;
    event(
        &tx,
        &session,
        "assistant_message_checkpointed",
        turn,
        Some(&message_id),
        Some(request),
        None,
    )?;
    tx.commit().map_err(|_| StoreError::Database)
}
pub(super) fn complete_request(
    connection: &Connection,
    turn: &str,
    completion: RequestCompletion,
) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let session = running(&tx, turn)?;
    let request = &completion.request_id;
    let message_id: String = tx
        .query_row(
            "SELECT message_id FROM model_requests WHERE id=?1 AND turn_id=?2 AND status='running'",
            rusqlite::params![request, turn],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::Conflict)?;
    let response = completion.response;
    let finish = serde_json::to_value(&response.finish_reason).map_err(|_| StoreError::Database)?;
    let incomplete = response.finish_reason == inference::FinishReason::OutputLimit;
    let status = if incomplete {
        "incomplete"
    } else {
        "completed"
    };
    tx.execute(
        "UPDATE messages SET text=?1,blocks=?2,continuation=?3,status=?4 WHERE id=?5",
        rusqlite::params![
            response.message.visible_text(),
            canonical(&response.message.blocks)?,
            response
                .message
                .continuation
                .as_ref()
                .map(canonical)
                .transpose()?,
            status,
            message_id
        ],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute("UPDATE model_requests SET status=?1,resolved_model=?2,finish_reason=?3,usage=?4,error_code=?5 WHERE id=?6",rusqlite::params![status,response.resolved_model,finish.as_str(),canonical(&response.usage)?,if incomplete{Some("provider_incomplete")}else{None},request]).map_err(|_|StoreError::Database)?;
    event(
        &tx,
        &session,
        "assistant_message_completed",
        turn,
        Some(&message_id),
        Some(request),
        None,
    )?;
    event(
        &tx,
        &session,
        "model_request_finished",
        turn,
        Some(&message_id),
        Some(request),
        None,
    )?;
    for intent in completion.tools {
        if intent.request_id != *request {
            return Err(StoreError::Invalid);
        }
        tx.execute("INSERT INTO tool_invocations(id,turn_id,request_id,call_id,provider_call_id,result_message_id,name,arguments,status) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'intent')",rusqlite::params![intent.id,turn,request,intent.call_id,intent.provider_call_id,intent.result_message_id,intent.name,canonical(&intent.arguments)?]).map_err(|_|StoreError::Database)?;
        event(
            &tx,
            &session,
            "tool_invocation_recorded",
            turn,
            None,
            Some(request),
            Some(&intent.id),
        )?;
    }
    tx.commit().map_err(|_| StoreError::Database)
}
pub(super) fn fail_request(
    connection: &Connection,
    turn: &str,
    request: &str,
    code: &str,
) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let (session,message,status):(String,String,String)=tx.query_row("SELECT t.session_id,r.message_id,r.status FROM model_requests r JOIN turns t ON t.id=r.turn_id WHERE r.id=?1 AND r.turn_id=?2",rusqlite::params![request,turn],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(|_|StoreError::Database)?;
    if status != "running" {
        return Ok(());
    }
    let status = if matches!(
        code,
        "cancelled" | "daemon_shutdown" | "steering_interrupted"
    ) {
        "interrupted"
    } else {
        "failed"
    };
    tx.execute(
        "UPDATE model_requests SET status=?1,error_code=?2 WHERE id=?3",
        rusqlite::params![status, code, request],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute(
        "UPDATE messages SET status=?1,continuation=NULL WHERE id=?2",
        rusqlite::params![status, message],
    )
    .map_err(|_| StoreError::Database)?;
    event(
        &tx,
        &session,
        "model_request_finished",
        turn,
        Some(&message),
        Some(request),
        None,
    )?;
    tx.commit().map_err(|_| StoreError::Database)
}
pub(super) fn start_tool(
    connection: &Connection,
    turn: &str,
    intent: &ToolIntent,
) -> StoreResult<bool> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let Some(session) = running_for_step(&tx, turn)? else {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    };
    let yielding: bool = tx
        .query_row(
            "SELECT pause_requested!=0 OR EXISTS(SELECT 1 FROM steering_instructions WHERE turn_id=?1 AND status='accepted') OR EXISTS(SELECT 1 FROM child_waits w WHERE w.parent_turn_id=?1 AND w.completed=0) FROM turns WHERE id=?1",
            [turn],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    if yielding {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    }
    if !super::governance::reserve(&tx, turn, false)? {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(false);
    }
    tx.execute("UPDATE tool_invocations SET status='running' WHERE id=?1 AND turn_id=?2 AND status='intent'",rusqlite::params![intent.id,turn]).map_err(|_|StoreError::Database)?;
    if tx.changes() != 1 {
        return Err(StoreError::Conflict);
    }
    event(
        &tx,
        &session,
        "tool_started",
        turn,
        None,
        Some(&intent.request_id),
        Some(&intent.id),
    )?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(true)
}
pub(super) fn finish_tool_tx(
    tx: &Transaction<'_>,
    session: &str,
    turn: &str,
    intent: &ToolIntent,
    outcome: &ToolOutcome,
) -> StoreResult<()> {
    let encoded = canonical(outcome)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT outcome FROM tool_invocations WHERE id=?1 AND turn_id=?2",
            rusqlite::params![intent.id, turn],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::NotFound)?;
    if let Some(existing) = existing {
        return if existing == encoded {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        };
    }
    for artifact in &outcome.artifacts {
        tx.execute(
            "INSERT OR IGNORE INTO artifacts(id,sha256,size_bytes,media_type) VALUES(?1,?2,?3,?4)",
            rusqlite::params![
                artifact.id,
                artifact.sha256,
                artifact.size_bytes as i64,
                artifact.media_type
            ],
        )
        .map_err(|_| StoreError::Database)?;
    }
    let block = ContentBlock {
        id: format!("{}:result", intent.id),
        content: BlockContent::ToolResult {
            call_id: intent.call_id.clone(),
            provider_call_id: intent.provider_call_id.clone(),
            is_error: outcome.status != "completed",
            output: outcome.output.clone(),
            artifact_ids: outcome.artifacts.iter().map(|a| a.id.clone()).collect(),
            effects_unknown: outcome.effects_unknown,
        },
    };
    let ordinal = next_message_ordinal(tx, turn)?;
    tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,blocks,request_id) VALUES(?1,?2,?3,?4,'tool',?5,'completed',?6,?7)",rusqlite::params![intent.result_message_id,session,turn,ordinal,outcome.output,canonical(&vec![block])?,intent.request_id]).map_err(|_|StoreError::Database)?;
    tx.execute(
        "UPDATE tool_invocations SET status=?1,outcome=?2 WHERE id=?3",
        rusqlite::params![outcome.status, encoded, intent.id],
    )
    .map_err(|_| StoreError::Database)?;
    event(
        tx,
        session,
        if outcome.status == "completed" {
            "tool_completed"
        } else {
            "tool_failed"
        },
        turn,
        Some(&intent.result_message_id),
        Some(&intent.request_id),
        Some(&intent.id),
    )?;
    Ok(())
}
pub(super) fn finish_tool(
    connection: &Connection,
    turn: &str,
    intent: &ToolIntent,
    outcome: ToolOutcome,
) -> StoreResult<()> {
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    finish_tool_tx(&tx, &session, turn, intent, &outcome)?;
    tx.commit().map_err(|_| StoreError::Database)
}
pub(super) fn reconcile_requests(tx: &Transaction<'_>, turn: &str, cause: &str) -> StoreResult<()> {
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    let mut statement = tx
        .prepare("SELECT id,message_id FROM model_requests WHERE turn_id=?1 AND status='running'")
        .map_err(|_| StoreError::Database)?;
    let rows = statement
        .query_map([turn], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|_| StoreError::Database)?;
    let unfinished: Vec<_> = rows
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| StoreError::Database)?;
    drop(statement);
    for (request, message) in unfinished {
        tx.execute(
            "UPDATE model_requests SET status='interrupted',error_code=?1 WHERE id=?2",
            rusqlite::params![cause, request],
        )
        .map_err(|_| StoreError::Database)?;
        tx.execute(
            "UPDATE messages SET status='interrupted',continuation=NULL WHERE id=?1",
            [&message],
        )
        .map_err(|_| StoreError::Database)?;
        event(
            tx,
            &session,
            "model_request_finished",
            turn,
            Some(&message),
            Some(&request),
            None,
        )?;
    }
    Ok(())
}

pub(super) fn reconcile_tools(tx: &Transaction<'_>, turn: &str, cause: &str) -> StoreResult<()> {
    let session: String = tx
        .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
            r.get(0)
        })
        .map_err(|_| StoreError::Database)?;
    let mut statement=tx.prepare("SELECT id,request_id,result_message_id,call_id,provider_call_id,name,arguments,status FROM tool_invocations WHERE turn_id=?1 AND outcome IS NULL ORDER BY rowid").map_err(|_|StoreError::Database)?;
    let rows = statement
        .query_map([turn], |r| {
            Ok((
                ToolIntent {
                    id: r.get(0)?,
                    request_id: r.get(1)?,
                    result_message_id: r.get(2)?,
                    call_id: r.get(3)?,
                    provider_call_id: r.get(4)?,
                    name: r.get(5)?,
                    arguments: json_column(r, 6)?,
                },
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(|_| StoreError::Database)?;
    let intents: Vec<_> = rows
        .collect::<rusqlite::Result<_>>()
        .map_err(|_| StoreError::Database)?;
    drop(statement);
    for (intent, status) in intents {
        if status == "waiting"
            && matches!(
                cause,
                "awaiting_children" | "steering_applied_before_dispatch"
            )
        {
            continue;
        }
        if status == "waiting" {
            tx.execute(
                "UPDATE child_waits SET completed=1 WHERE invocation_id=?1",
                [&intent.id],
            )
            .map_err(|_| StoreError::Database)?;
        }
        let mut outcome = ToolOutcome::failed(cause, status == "running");
        outcome.output = if status == "running" {
            format!(
                "The tool was interrupted ({cause}); external effects may be unknown. The invocation was not replayed."
            )
        } else {
            format!("The tool was not launched ({cause}); the invocation was not replayed.")
        };
        finish_tool_tx(tx, &session, turn, &intent, &outcome)?;
    }
    Ok(())
}

pub(super) fn claim_next_turn(
    connection: &Connection,
) -> StoreResult<Option<slop_runtime::chat::TurnWork>> {
    use slop_runtime::chat::{ContextMessage, RoleKind, TurnWork};
    let tx = connection
        .unchecked_transaction()
        .map_err(|_| StoreError::Database)?;
    super::governance::prepare_groups(&tx)?;
    type Queued = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
    );
    let next:Option<Queued>=tx.query_row("SELECT t.id,t.session_id,t.requested_model,t.settings,t.requested_settings,s.execution,t.resume_count,t.usage_unknown,t.input_tokens,t.output_tokens,t.total_tokens,t.total_source,(SELECT COALESCE(MAX(ordinal),0) FROM model_requests WHERE turn_id=t.id) FROM turns t JOIN sessions s ON s.id=t.session_id WHERE t.status='queued'
        AND NOT EXISTS(SELECT 1 FROM turns active WHERE active.session_id=t.session_id AND active.status='running')
        AND NOT EXISTS(SELECT 1 FROM turns earlier WHERE earlier.session_id=t.session_id AND earlier.ordinal<t.ordinal AND earlier.status IN('queued','running','paused','awaiting_children'))
        AND (s.workspace_root IS NULL OR NOT EXISTS(SELECT 1 FROM turns busy JOIN sessions bs ON bs.id=busy.session_id WHERE busy.status='running' AND bs.workspace_root=s.workspace_root))
        AND NOT EXISTS(SELECT 1 FROM runs r JOIN tasks k ON k.id=r.task_id JOIN batches b ON b.id=k.admission_batch_id WHERE r.turn_id=t.id AND (SELECT count(*) FROM runs br JOIN tasks bk ON bk.id=br.task_id JOIN turns bt ON bt.id=br.turn_id WHERE bk.admission_batch_id=b.id AND bt.status='running')>=b.max_concurrent_runs)
        ORDER BY (SELECT last_admission FROM admission_groups WHERE id=COALESCE((SELECT 'batch:'||k.admission_batch_id FROM runs r JOIN tasks k ON k.id=r.task_id WHERE r.turn_id=t.id),(SELECT 'task:'||k.root_task_id FROM runs r JOIN tasks k ON k.id=r.task_id WHERE r.turn_id=t.id),'session:'||t.session_id)),t.rowid LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?,r.get(12)?))).optional().map_err(|_|StoreError::Database)?;
    let Some((
        turn_id,
        session_id,
        requested_model,
        settings,
        requested_settings,
        execution,
        resume_count,
        usage_unknown,
        input,
        output,
        total,
        total_source,
        previous_ordinal,
    )) = next
    else {
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(None);
    };
    let settings: inference::GenerationSettings = decode(&settings)?;
    let requested_settings = decode(&requested_settings)?;
    let execution: Option<WorkspacePolicy> = execution.map(|value| decode(&value)).transpose()?;
    let steering_count: i64 = tx
        .query_row(
            "SELECT count(*) FROM steering_instructions WHERE turn_id=?1 AND status IN ('accepted','applied')",
            [&turn_id],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let tool_calls_used: i64 = tx
        .query_row(
            "SELECT count(*) FROM tool_invocations WHERE turn_id=?1",
            [&turn_id],
            |row| row.get(0),
        )
        .map_err(|_| StoreError::Database)?;
    let orchestration:Option<slop_runtime::orchestration::OrchestrationPolicy>=tx.query_row("SELECT json_extract(k.spec,'$.orchestration') FROM runs r JOIN tasks k ON k.id=r.task_id WHERE r.turn_id=?1",[&turn_id],|r|r.get::<_,Option<String>>(0)).optional().map_err(|_|StoreError::Database)?.flatten().map(|s|decode(&s)).transpose()?;
    let base_request_limit = execution
        .as_ref()
        .map_or(1_i64, |policy| i64::from(policy.max_model_requests));
    // Delegation has a bounded continuation allowance under aggregate budgets.
    // Other workspace turns retain their policy cap; plain text retains its
    // one-request default with a bounded steering/resume allowance.
    let max_model_requests = if orchestration.is_some() {
        64
    } else if execution.is_some() {
        base_request_limit.clamp(1, 64) as u32
    } else {
        base_request_limit
            .saturating_add(steering_count)
            .saturating_add(resume_count)
            .clamp(1, 64) as u32
    };
    let initial_usage = if usage_unknown {
        slop_runtime::providers::Usage::default()
    } else {
        slop_runtime::providers::Usage {
            input_tokens: input.and_then(|value| value.parse().ok()),
            output_tokens: output.and_then(|value| value.parse().ok()),
            total_tokens: total.and_then(|value| value.parse().ok()),
            total_source: total_source.and_then(|value| match value.as_str() {
                "reported" => Some(slop_runtime::providers::UsageSource::Reported),
                "derived" => Some(slop_runtime::providers::UsageSource::Derived),
                _ => None,
            }),
        }
    };
    let mut statement=tx.prepare("SELECT m.id,m.role,m.text,m.blocks,m.continuation FROM messages m JOIN turns t ON t.id=m.turn_id
        WHERE m.session_id=?1 AND m.status='completed' AND (t.status='completed' OR m.turn_id=?2 OR (t.status IN('failed','cancelled','interrupted','incomplete') AND EXISTS(SELECT 1 FROM tool_invocations i WHERE i.turn_id=t.id)))
        ORDER BY m.ordinal LIMIT ?3").map_err(|_|StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![
            session_id,
            turn_id,
            (MAX_CONTEXT_MESSAGES + 1) as i64
        ])
        .map_err(|_| StoreError::Database)?;
    let mut history = Vec::new();
    let mut messages = Vec::new();
    let mut bytes = 0usize;
    let mut overflow = false;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if history.len() >= MAX_CONTEXT_MESSAGES {
            overflow = true;
            break;
        }
        let id: String = row.get(0).map_err(|_| StoreError::Database)?;
        let role: String = row.get(1).map_err(|_| StoreError::Database)?;
        let text: String = row.get(2).map_err(|_| StoreError::Database)?;
        let mut blocks: Vec<ContentBlock> =
            json_column(row, 3).map_err(|_| StoreError::Database)?;
        if blocks.is_empty() {
            blocks.push(ContentBlock {
                id: format!("{id}:block:0"),
                content: BlockContent::Text { text: text.clone() },
            });
        }
        let continuation = optional_json_column(row, 4).map_err(|_| StoreError::Database)?;
        let role = match role.as_str() {
            "user" => MessageRole::User,
            "assistant" => MessageRole::Assistant,
            "tool" => MessageRole::Tool,
            _ => return Err(StoreError::Database),
        };
        if role != MessageRole::Tool {
            messages.push(ContextMessage {
                role: if role == MessageRole::User {
                    RoleKind::User
                } else {
                    RoleKind::Assistant
                },
                text,
            });
        }
        let message = InferenceMessage {
            role,
            blocks,
            continuation,
        };
        bytes = bytes.saturating_add(canonical(&message)?.len());
        if bytes > MAX_CONTEXT_BYTES {
            overflow = true;
            break;
        }
        history.push(message);
    }
    drop(rows);
    drop(statement);
    if overflow {
        tx.execute("UPDATE turns SET status='failed',error_code='context_limit',error_message='Conversation context exceeds the supported bound.' WHERE id=?1",[&turn_id]).map_err(|_|StoreError::Database)?;
        append_event(&tx, &session_id, "turn_failed", Some(&turn_id), None)?;
        tx.commit().map_err(|_| StoreError::Database)?;
        return Ok(None);
    }
    tx.execute(
        "UPDATE turns SET status='running' WHERE id=?1 AND status='queued'",
        [&turn_id],
    )
    .map_err(|_| StoreError::Database)?;
    super::governance::admitted(&tx, &turn_id)?;
    append_event(&tx, &session_id, "turn_started", Some(&turn_id), None)?;
    tx.commit().map_err(|_| StoreError::Database)?;
    Ok(Some(TurnWork {
        turn_id,
        session_id,
        requested_model,
        max_tokens: settings.max_output_tokens,
        messages,
        history,
        settings,
        requested_settings,
        execution,
        orchestration,
        next_request_ordinal: previous_ordinal.saturating_add(1).max(1) as u32,
        max_model_requests,
        tool_calls_used: tool_calls_used as u32,
        initial_usage,
        resume_count: resume_count.clamp(0, u32::MAX as i64) as u32,
    }))
}

fn ids(connection: &Connection, sql: &str, id: &str) -> StoreResult<Vec<String>> {
    connection
        .prepare(sql)
        .map_err(|_| StoreError::Database)?
        .query_map([id], |r| r.get(0))
        .map_err(|_| StoreError::Database)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|_| StoreError::Database)
}
pub(super) fn get_turn(connection: &Connection, id: &str) -> StoreResult<TurnResponse> {
    let mut turn = get_base_turn(connection, id)?;
    turn.settings = connection
        .query_row("SELECT settings FROM turns WHERE id=?1", [id], |r| {
            json_column(r, 0)
        })
        .map_err(|_| StoreError::Database)?;
    turn.model_request_ids = ids(
        connection,
        "SELECT id FROM model_requests WHERE turn_id=?1 ORDER BY ordinal",
        id,
    )?;
    turn.tool_invocation_ids = ids(
        connection,
        "SELECT id FROM tool_invocations WHERE turn_id=?1 ORDER BY rowid",
        id,
    )?;
    Ok(turn)
}
fn public_usage(value: slop_runtime::providers::Usage) -> UsageResponse {
    UsageResponse {
        input_tokens: value.input_tokens,
        output_tokens: value.output_tokens,
        total_tokens: value.total_tokens,
        total_source: value.total_source.map(|source| {
            match source {
                slop_runtime::providers::UsageSource::Reported => "reported",
                slop_runtime::providers::UsageSource::Derived => "derived",
            }
            .to_owned()
        }),
    }
}
fn request_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<wire::ModelRequestResponse> {
    let usage: Option<slop_runtime::providers::Usage> = optional_json_column(r, 10)?;
    Ok(wire::ModelRequestResponse {
        id: r.get(0)?,
        turn_id: r.get(1)?,
        message_id: r.get(2)?,
        ordinal: r.get(3)?,
        requested_model: r.get(4)?,
        resolved_model: r.get(5)?,
        requested_settings: json_column(r, 6)?,
        effective_settings: json_column(r, 7)?,
        status: r.get(8)?,
        finish_reason: r.get(9)?,
        usage: usage.map(public_usage),
        error_code: r.get(11)?,
    })
}
fn tool_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<wire::ToolInvocationResponse> {
    let outcome: Option<ToolOutcome> = optional_json_column(r, 7)?;
    Ok(wire::ToolInvocationResponse {
        id: r.get(0)?,
        turn_id: r.get(1)?,
        request_id: r.get(2)?,
        call_id: r.get(3)?,
        name: r.get(4)?,
        arguments: json_column(r, 5)?,
        status: r.get(6)?,
        output: outcome.as_ref().map(|o| o.output.clone()),
        artifact_ids: outcome
            .as_ref()
            .map(|o| o.artifacts.iter().map(|a| a.id.clone()).collect())
            .unwrap_or_default(),
        error_code: outcome.as_ref().and_then(|o| o.error_code.clone()),
        effects_unknown: outcome.is_some_and(|o| o.effects_unknown),
    })
}
fn paginated<T>(
    connection: &Connection,
    turn: &str,
    after: u64,
    limit: usize,
    sql: &str,
    decode: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    cursor_index: usize,
) -> StoreResult<Page<T>> {
    get_turn(connection, turn)?;
    let mut statement = connection.prepare(sql).map_err(|_| StoreError::Database)?;
    let mut rows = statement
        .query(rusqlite::params![
            turn,
            after.min(i64::MAX as u64) as i64,
            (limit + 1) as i64
        ])
        .map_err(|_| StoreError::Database)?;
    let mut items = Vec::new();
    let mut cursor = None;
    let mut more = false;
    while let Some(row) = rows.next().map_err(|_| StoreError::Database)? {
        if items.len() == limit {
            more = true;
            break;
        }
        cursor = Some(
            row.get::<_, i64>(cursor_index)
                .map_err(|_| StoreError::Database)? as u64,
        );
        items.push(decode(row).map_err(|_| StoreError::Database)?);
    }
    Ok(Page {
        items,
        next_after: if more { cursor } else { None },
    })
}

impl StoreClient {
    pub async fn model_requests(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<wire::ModelRequestResponse>> {
        let turn = turn.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move|c|paginated(c,&turn,after.unwrap_or(0),limit,"SELECT id,turn_id,message_id,ordinal,requested_model,resolved_model,requested_settings,settings,status,finish_reason,usage,error_code FROM model_requests WHERE turn_id=?1 AND ordinal>?2 ORDER BY ordinal LIMIT ?3",request_from_row,3)).await
    }
    pub async fn tool_invocations(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: usize,
    ) -> StoreResult<Page<wire::ToolInvocationResponse>> {
        let turn = turn.to_owned();
        let limit = page_limit(limit)?;
        self.submit(move|c|paginated(c,&turn,after.unwrap_or(0),limit,"SELECT id,turn_id,request_id,call_id,name,arguments,status,outcome,rowid FROM tool_invocations WHERE turn_id=?1 AND rowid>?2 ORDER BY rowid LIMIT ?3",tool_from_row,8)).await
    }
    pub async fn tool_invocation(&self, id: &str) -> StoreResult<wire::ToolInvocationResponse> {
        let id = id.to_owned();
        self.submit(move|c|c.query_row("SELECT id,turn_id,request_id,call_id,name,arguments,status,outcome FROM tool_invocations WHERE id=?1",[id],tool_from_row).optional().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)).await
    }
    pub async fn message(&self, id: &str) -> StoreResult<MessageResponse> {
        let id = id.to_owned();
        self.submit(move|c|{
            let mut statement=c.prepare("SELECT id,session_id,turn_id,role,text,status,blocks,request_id FROM messages WHERE id=?1").map_err(|_|StoreError::Database)?;
            let mut rows=statement.query([id]).map_err(|_|StoreError::Database)?;let row=rows.next().map_err(|_|StoreError::Database)?.ok_or(StoreError::NotFound)?;
            Ok(MessageResponse{id:row.get(0).map_err(|_|StoreError::Database)?,session_id:row.get(1).map_err(|_|StoreError::Database)?,turn_id:row.get(2).map_err(|_|StoreError::Database)?,role:row.get(3).map_err(|_|StoreError::Database)?,text:row.get(4).map_err(|_|StoreError::Database)?,status:row.get(5).map_err(|_|StoreError::Database)?,blocks:public_blocks_from_row(row,6,0,4)?,request_id:row.get(7).map_err(|_|StoreError::Database)?})
        }).await
    }
    pub async fn artifact(&self, id: &str) -> StoreResult<wire::ArtifactResponse> {
        let id = id.to_owned();
        self.submit(move |c| {
            c.query_row(
                "SELECT id,sha256,size_bytes,media_type FROM artifacts WHERE id=?1",
                [id],
                |r| {
                    Ok(wire::ArtifactResponse {
                        id: r.get(0)?,
                        sha256: r.get(1)?,
                        size_bytes: r.get::<_, i64>(2)? as u64,
                        media_type: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)
        })
        .await
    }
}
