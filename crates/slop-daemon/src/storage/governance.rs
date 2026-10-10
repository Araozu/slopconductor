//! Durable operation accounting and admission groups.
use super::*;
use slop_protocol::orchestration::{BudgetUsage, OperationBudget};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch("ALTER TABLE batches ADD COLUMN cancellation_requested INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE turns ADD COLUMN budget_exhausted INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE turns ADD COLUMN admission_epoch INTEGER NOT NULL DEFAULT -1;
        CREATE TRIGGER readmission_epoch AFTER UPDATE OF status ON turns
            WHEN NEW.status='queued' AND OLD.status!='queued'
            BEGIN UPDATE turns SET admission_epoch=-1 WHERE id=NEW.id; END;
        CREATE TABLE operation_budgets(scope TEXT PRIMARY KEY, max_model_requests INTEGER,
            max_tool_calls INTEGER, model_requests INTEGER NOT NULL DEFAULT 0,
            tool_calls INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE admission_clock(singleton INTEGER PRIMARY KEY CHECK(singleton=1), sequence INTEGER NOT NULL);
        INSERT INTO admission_clock VALUES(1,0);
        CREATE TABLE admission_groups(id TEXT PRIMARY KEY, last_admission INTEGER NOT NULL);
        INSERT INTO operation_budgets(scope,model_requests,tool_calls)
            SELECT 'task:'||k.id,
                (SELECT count(*) FROM model_requests m JOIN runs r ON r.turn_id=m.turn_id WHERE r.task_id=k.id),
                (SELECT count(*) FROM tool_invocations i JOIN runs r ON r.turn_id=i.turn_id WHERE r.task_id=k.id AND EXISTS(SELECT 1 FROM session_events e WHERE e.invocation_id=i.id AND e.kind='tool_started')) FROM tasks k;
        INSERT INTO operation_budgets(scope,model_requests,tool_calls)
            SELECT 'batch:'||b.id,
                (SELECT count(*) FROM model_requests m JOIN runs r ON r.turn_id=m.turn_id JOIN tasks k ON k.id=r.task_id WHERE k.batch_id=b.id),
                (SELECT count(*) FROM tool_invocations i JOIN runs r ON r.turn_id=i.turn_id JOIN tasks k ON k.id=r.task_id WHERE k.batch_id=b.id AND EXISTS(SELECT 1 FROM session_events e WHERE e.invocation_id=i.id AND e.kind='tool_started')) FROM batches b;")
        .map_err(|_|StoreError::Database)
}

pub(crate) fn validate(budget: Option<&OperationBudget>) -> StoreResult<()> {
    if budget.is_some_and(|b| b.max_model_requests > 1_000_000 || b.max_tool_calls > 1_000_000) {
        return Err(StoreError::Invalid);
    }
    Ok(())
}

pub(super) fn register(
    tx: &Transaction<'_>,
    scope: &str,
    budget: Option<&OperationBudget>,
) -> StoreResult<()> {
    validate(budget)?;
    tx.execute(
        "INSERT INTO operation_budgets(scope,max_model_requests,max_tool_calls) VALUES(?1,?2,?3)",
        rusqlite::params![
            scope,
            budget.map(|b| b.max_model_requests),
            budget.map(|b| b.max_tool_calls)
        ],
    )
    .map_err(|_| StoreError::Database)?;
    Ok(())
}

pub(super) fn usage(c: &Connection, scope: &str) -> StoreResult<BudgetUsage> {
    c.query_row(
        "SELECT model_requests,tool_calls FROM operation_budgets WHERE scope=?1",
        [scope],
        |r| {
            Ok(BudgetUsage {
                model_requests: r.get::<_, i64>(0)? as u64,
                tool_calls: r.get::<_, i64>(1)? as u64,
            })
        },
    )
    .map_err(|_| StoreError::Database)
}

/// Count accepted dispatches, including uncertain outcomes; never refund a reservation.
pub(super) fn reserve(tx: &Transaction<'_>, turn: &str, model: bool) -> StoreResult<bool> {
    let mut statement = tx.prepare("WITH RECURSIVE ancestors(id) AS (SELECT task_id FROM runs WHERE turn_id=?1 UNION ALL SELECT l.parent_task_id FROM child_links l JOIN ancestors a ON a.id=l.child_task_id)
        SELECT 'task:'||id FROM ancestors UNION SELECT 'batch:'||k.admission_batch_id FROM runs r JOIN tasks k ON k.id=r.task_id WHERE r.turn_id=?1 AND k.admission_batch_id IS NOT NULL")
        .map_err(|_|StoreError::Database)?;
    let scopes = statement
        .query_map([turn], |r| r.get::<_, String>(0))
        .map_err(|_| StoreError::Database)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| StoreError::Database)?;
    drop(statement);
    let (used, cap) = if model {
        ("model_requests", "max_model_requests")
    } else {
        ("tool_calls", "max_tool_calls")
    };
    for scope in &scopes {
        let exhausted: bool = tx.query_row(&format!("SELECT {cap} IS NOT NULL AND {used}>={cap} FROM operation_budgets WHERE scope=?1"),[scope],|r|r.get(0)).map_err(|_|StoreError::Database)?;
        if exhausted {
            tx.execute("UPDATE turns SET budget_exhausted=1 WHERE id=?1", [turn])
                .map_err(|_| StoreError::Database)?;
            let session: String = tx
                .query_row("SELECT session_id FROM turns WHERE id=?1", [turn], |r| {
                    r.get(0)
                })
                .map_err(|_| StoreError::Database)?;
            append_event(tx, &session, "operation_budget_exhausted", Some(turn), None)?;
            return Ok(false);
        }
    }
    for scope in scopes {
        tx.execute(
            &format!("UPDATE operation_budgets SET {used}={used}+1 WHERE scope=?1"),
            [scope],
        )
        .map_err(|_| StoreError::Database)?;
    }
    Ok(true)
}

pub(super) fn prepare_groups(tx: &Transaction<'_>) -> StoreResult<()> {
    // New groups join at the current clock, so a stream of arrivals cannot jump
    // ahead of a group already waiting. FIFO ordering is retained inside groups.
    tx.execute("UPDATE turns SET admission_epoch=(SELECT sequence FROM admission_clock WHERE singleton=1) WHERE status='queued' AND admission_epoch=-1",[]).map_err(|_|StoreError::Database)?;
    tx.execute("INSERT OR IGNORE INTO admission_groups(id,last_admission)
        SELECT COALESCE('batch:'||k.admission_batch_id,'task:'||k.root_task_id,'session:'||t.session_id),
            (SELECT sequence FROM admission_clock WHERE singleton=1)
        FROM turns t LEFT JOIN runs r ON r.turn_id=t.id LEFT JOIN tasks k ON k.id=r.task_id WHERE t.status='queued'",[]).map_err(|_|StoreError::Database)?;
    // A returning idle group joins with its new queue's arrival, rather than
    // reusing an old ticket to jump ahead of work already waiting.
    tx.execute("UPDATE admission_groups SET last_admission=MAX(last_admission,
        (SELECT MIN(t.admission_epoch) FROM turns t LEFT JOIN runs r ON r.turn_id=t.id LEFT JOIN tasks k ON k.id=r.task_id
            WHERE t.status='queued' AND COALESCE('batch:'||k.admission_batch_id,'task:'||k.root_task_id,'session:'||t.session_id)=admission_groups.id))
        WHERE id IN(SELECT COALESCE('batch:'||k.admission_batch_id,'task:'||k.root_task_id,'session:'||t.session_id)
            FROM turns t LEFT JOIN runs r ON r.turn_id=t.id LEFT JOIN tasks k ON k.id=r.task_id WHERE t.status='queued')",[]).map_err(|_|StoreError::Database)?;
    Ok(())
}

pub(super) fn admitted(tx: &Transaction<'_>, turn: &str) -> StoreResult<()> {
    tx.execute(
        "UPDATE admission_clock SET sequence=sequence+1 WHERE singleton=1",
        [],
    )
    .map_err(|_| StoreError::Database)?;
    tx.execute("UPDATE admission_groups SET last_admission=(SELECT sequence FROM admission_clock WHERE singleton=1)
        WHERE id=(SELECT COALESCE('batch:'||k.admission_batch_id,'task:'||k.root_task_id,'session:'||t.session_id)
            FROM turns t LEFT JOIN runs r ON r.turn_id=t.id LEFT JOIN tasks k ON k.id=r.task_id WHERE t.id=?1)",[turn]).map_err(|_|StoreError::Database)?;
    Ok(())
}
