//! One persistent conversation owns messages, execution turns, policy, and lineage.
use super::*;

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE conversation_ids AS
            SELECT r.task_id,r.id AS run_id,r.session_id,r.turn_id FROM runs r;
        ALTER TABLE sessions ADD COLUMN spec TEXT;
        ALTER TABLE sessions ADD COLUMN requested_spec TEXT;
        ALTER TABLE sessions ADD COLUMN batch_id TEXT REFERENCES batches(id);
        ALTER TABLE sessions ADD COLUMN combination_index INTEGER;
        ALTER TABLE sessions ADD COLUMN root_task_id TEXT REFERENCES sessions(id);
        ALTER TABLE sessions ADD COLUMN admission_batch_id TEXT REFERENCES batches(id);
        ALTER TABLE turns ADD COLUMN retry_of TEXT REFERENCES turns(id);
        UPDATE sessions SET
            spec=COALESCE((SELECT k.spec FROM tasks k JOIN runs r ON r.task_id=k.id WHERE r.session_id=sessions.id),
                json_object('title',title,'prompt','','model',provider||'/'||model,'settings',json(settings),'project',NULL)),
            requested_spec=COALESCE((SELECT k.requested_spec FROM tasks k JOIN runs r ON r.task_id=k.id WHERE r.session_id=sessions.id),
                json_object('title',title,'prompt','','model',provider||'/'||model,'settings',json(COALESCE((SELECT json_extract(payload,'$.settings') FROM commands WHERE scope='create_session' AND json_extract(receipt,'$.session_id')=sessions.id LIMIT 1),'{}')),'project',NULL)),
            root_task_id=COALESCE((SELECT root.session_id FROM tasks k JOIN runs r ON r.task_id=k.id
                JOIN runs root ON root.task_id=k.root_task_id WHERE r.session_id=sessions.id ORDER BY root.attempt DESC LIMIT 1),id),
            admission_batch_id=(SELECT k.admission_batch_id FROM tasks k JOIN runs r ON r.task_id=k.id WHERE r.session_id=sessions.id),
            batch_id=(SELECT k.batch_id FROM tasks k JOIN runs r ON r.task_id=k.id WHERE r.session_id=sessions.id
                AND r.attempt=(SELECT MAX(attempt) FROM runs WHERE task_id=k.id)),
            combination_index=(SELECT k.combination_index FROM tasks k JOIN runs r ON r.task_id=k.id WHERE r.session_id=sessions.id
                AND r.attempt=(SELECT MAX(attempt) FROM runs WHERE task_id=k.id));
        UPDATE turns SET retry_of=(SELECT prior.turn_id FROM runs r JOIN runs prior ON prior.id=r.retry_of WHERE r.turn_id=turns.id);
        CREATE TABLE conversation_children(
            child_task_id TEXT PRIMARY KEY REFERENCES sessions(id),
            parent_task_id TEXT NOT NULL REFERENCES sessions(id),
            parent_run_id TEXT NOT NULL REFERENCES turns(id),
            root_task_id TEXT NOT NULL REFERENCES sessions(id),
            depth INTEGER NOT NULL CHECK(depth BETWEEN 1 AND 4),
            cancel_with_parent INTEGER NOT NULL,context TEXT NOT NULL);
        INSERT INTO conversation_children
            SELECT child.session_id,parent.session_id,parent.turn_id,
                (SELECT root_task_id FROM sessions WHERE id=child.session_id),l.depth,l.cancel_with_parent,l.context
            FROM child_links l JOIN conversation_ids child ON child.task_id=l.child_task_id
                JOIN conversation_ids parent ON parent.run_id=l.parent_run_id;
        DROP TABLE child_links;
        ALTER TABLE conversation_children RENAME TO child_links;
        CREATE INDEX children_parent ON child_links(parent_run_id);
        CREATE INDEX children_root ON child_links(root_task_id);
        UPDATE child_waits SET child_run_ids=(SELECT json_group_array(COALESCE(m.turn_id,j.value))
            FROM json_each(child_waits.child_run_ids) j LEFT JOIN conversation_ids m ON m.run_id=j.value);
        INSERT INTO operation_budgets(scope,max_model_requests,max_tool_calls,model_requests,tool_calls)
            SELECT 'session:'||s.id,json_extract(s.spec,'$.budget.max_model_requests'),json_extract(s.spec,'$.budget.max_tool_calls'),
                (SELECT count(*) FROM model_requests m JOIN turns t ON t.id=m.turn_id WHERE t.session_id=s.id),
                (SELECT count(*) FROM tool_invocations i JOIN turns t ON t.id=i.turn_id WHERE t.session_id=s.id
                    AND EXISTS(SELECT 1 FROM session_events e WHERE e.invocation_id=i.id AND e.kind='tool_started')) FROM sessions s;
        DELETE FROM operation_budgets WHERE scope LIKE 'task:%';
        DELETE FROM admission_groups;
        DROP TABLE task_events;
        DROP TABLE runs;
        DROP TABLE tasks;
        DROP TABLE conversation_ids;
        CREATE UNIQUE INDEX session_batch_member ON sessions(batch_id,combination_index);",
    )
    .map_err(|_| StoreError::Database)
}

#[cfg(test)]
pub(super) fn legacy_fixture(c: &Connection, version: i64) -> StoreResult<()> {
    c.pragma_update(None, "foreign_keys", "OFF")
        .map_err(|_| StoreError::Database)?;
    let result = (|| {
        let tx = c
            .unchecked_transaction()
            .map_err(|_| StoreError::Database)?;
        tx.execute_batch("DROP INDEX messages_request;
            ALTER TABLE messages DROP COLUMN deleted;
            ALTER TABLE messages DROP COLUMN context_excluded;
            ALTER TABLE turns DROP COLUMN history_only;
            ALTER TABLE sessions DROP COLUMN fork;
            DROP TABLE session_context;
            ALTER TABLE model_requests DROP COLUMN context;
            ALTER TABLE sessions DROP COLUMN inbox;
            DROP TABLE conversation_drafts;
            CREATE TEMP TABLE saved_work AS SELECT id,owner_node_id,spec,requested_spec,created_order,last_event_sequence FROM sessions;
            DROP TABLE child_waits; DROP TABLE child_links;
            DROP TABLE admission_groups; DROP TABLE admission_clock; DROP TABLE operation_budgets;
            DROP TRIGGER readmission_epoch;
            ALTER TABLE turns DROP COLUMN admission_epoch; ALTER TABLE turns DROP COLUMN budget_exhausted;
            ALTER TABLE turns DROP COLUMN retry_of;
            DROP INDEX session_batch_member;
            ALTER TABLE sessions DROP COLUMN root_task_id; ALTER TABLE sessions DROP COLUMN admission_batch_id;
            ALTER TABLE sessions DROP COLUMN spec; ALTER TABLE sessions DROP COLUMN requested_spec;
            ALTER TABLE sessions DROP COLUMN batch_id; ALTER TABLE sessions DROP COLUMN combination_index;
            DROP TABLE batches;") .map_err(|_| StoreError::Database)?;
        if version >= 6 {
            orchestration::migrate(&tx)?;
            tx.execute_batch("INSERT INTO tasks(id,owner_node_id,spec,requested_spec,created_order,last_event_sequence)
                    SELECT id,owner_node_id,spec,requested_spec,created_order,last_event_sequence FROM saved_work
                    WHERE EXISTS(SELECT 1 FROM turns WHERE session_id=saved_work.id);
                INSERT INTO runs(id,task_id,attempt,session_id,turn_id)
                    SELECT id,session_id,ordinal,session_id,id FROM turns;
                INSERT INTO task_events(task_id,sequence,run_id,kind,status,session_event_sequence)
                    SELECT e.session_id,e.sequence,e.turn_id,e.kind,t.status,e.sequence FROM session_events e
                    JOIN turns t ON t.id=e.turn_id;") .map_err(|_| StoreError::Database)?;
        }
        if version < 5 {
            tx.execute_batch(
                "ALTER TABLE sessions DROP COLUMN managed_workspace_id;
                DROP TABLE project_events; DROP TABLE managed_workspaces; DROP TABLE projects;",
            )
            .map_err(|_| StoreError::Database)?;
        }
        tx.execute_batch("DROP TABLE saved_work;")
            .map_err(|_| StoreError::Database)?;
        tx.pragma_update(None, "user_version", version)
            .map_err(|_| StoreError::Database)?;
        tx.commit().map_err(|_| StoreError::Database)
    })();
    c.pragma_update(None, "foreign_keys", "ON")
        .map_err(|_| StoreError::Database)?;
    result
}
