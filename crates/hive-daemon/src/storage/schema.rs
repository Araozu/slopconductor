use super::*;

pub(super) fn verify_regular_or_absent(path: &Path) -> StoreResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(StoreError::Configuration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(StoreError::Configuration),
    }
}

pub(super) fn migrate(
    connection: &Connection,
    existing_version: i64,
    node_name: Option<String>,
) -> StoreResult<()> {
    migrate_with_hook(connection, existing_version, node_name, |_| Ok(()))
}

pub(super) fn migrate_with_hook<F>(
    connection: &Connection,
    existing_version: i64,
    node_name: Option<String>,
    hook: F,
) -> StoreResult<()>
where
    F: FnOnce(&Transaction<'_>) -> StoreResult<()>,
{
    // Schema 9 folds job identities into their conversation and turn records.
    connection
        .pragma_update(None, "foreign_keys", "OFF")
        .map_err(|_| StoreError::Database)?;
    let result = (|| {
        let tx = connection
            .unchecked_transaction()
            .map_err(|_| StoreError::Database)?;
        if existing_version == 0 {
            tx.execute_batch(
                "CREATE TABLE node_identity (
                singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                node_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                os TEXT NOT NULL
            );",
            )
            .map_err(|_| StoreError::Database)?;
            let identity = node_name.unwrap_or_else(default_node_name);
            tx.execute(
                "INSERT INTO node_identity (singleton, node_id, name, os) VALUES (1, ?1, ?2, ?3)",
                rusqlite::params![random_node_id()?, identity, env::consts::OS],
            )
            .map_err(|_| StoreError::Database)?;
        } else if let Some(name) = node_name {
            let existing = query_node(&tx)?;
            validate_identity(&existing)?;
            tx.execute(
                "UPDATE node_identity SET name = ?1 WHERE singleton = 1",
                [name],
            )
            .map_err(|_| StoreError::Database)?;
            if tx.changes() != 1 {
                return Err(StoreError::Database);
            }
        }
        if existing_version <= 1 {
            create_chat_schema(&tx)?;
        }
        if existing_version < 3 {
            execution::migrate(&tx)?;
        }
        if existing_version < 4 {
            steering_migrate(&tx)?;
        }
        if existing_version < 5 {
            workspaces::migrate_legacy(&tx)?;
        }
        if existing_version < 6 {
            orchestration::migrate(&tx)?;
        }
        if existing_version < 7 {
            governance::migrate(&tx)?;
        }
        if existing_version < 8 {
            children::migrate(&tx)?;
        }
        if existing_version < 9 {
            conversations::migrate(&tx)?;
        }
        if existing_version < 10 {
            inbox::migrate(&tx)?;
        }
        if existing_version < 11 {
            context::migrate(&tx)?;
        }
        if existing_version < 12 {
            messages::migrate_message_models(&tx)?;
        }
        if existing_version < 13 {
            history::migrate(&tx)?;
        }
        if existing_version < 14 {
            execution::expand_message_order(&tx)?;
        }
        if existing_version < 15 {
            children::migrate_notifications(&tx)?;
            children::migrate_subagent_receipt_payloads(&tx)?;
        }
        if existing_version < 16 {
            children::migrate_wait_modes(&tx)?;
        }
        if existing_version < 17 {
            children::migrate_child_activity(&tx)?;
        }
        if existing_version < 18 {
            children::migrate_wait_results(&tx)?;
        }
        if existing_version < 19 {
            peers::migrate(&tx)?;
        }
        if existing_version < 20 {
            let has_name: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('managed_workspaces') WHERE name='name')",
                [],
                |row| row.get(0),
            ).map_err(|_| StoreError::Database)?;
            if !has_name {
                tx.execute_batch("ALTER TABLE managed_workspaces ADD COLUMN name TEXT NOT NULL DEFAULT ''; UPDATE managed_workspaces SET name=id;")
                    .map_err(|_| StoreError::Database)?;
            }
        }
        if existing_version < 21 {
            let has_name: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM pragma_table_info('projects') WHERE name='name')",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| StoreError::Database)?;
            if !has_name {
                tx.execute_batch("ALTER TABLE projects ADD COLUMN name TEXT NOT NULL DEFAULT ''; UPDATE projects SET name=id;")
                    .map_err(|_| StoreError::Database)?;
            }
        }
        if existing_version < 22 {
            workspaces::migrate(&tx)?;
        }
        if existing_version < 23 {
            queue::migrate(&tx)?;
        }
        if existing_version < 24 {
            workspaces::migrate_colors(&tx)?;
        }
        if existing_version < 25 {
            tx.execute_batch("ALTER TABLE sessions ADD COLUMN last_user_activity_at INTEGER;")
                .map_err(|_| StoreError::Database)?;
        }
        let broken: bool = tx
            .prepare("PRAGMA foreign_key_check")
            .map_err(|_| StoreError::Database)?
            .exists([])
            .map_err(|_| StoreError::Database)?;
        if broken {
            return Err(StoreError::Database);
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)
            .map_err(|_| StoreError::Database)?;
        hook(&tx)?;
        tx.commit().map_err(|_| StoreError::Database)
    })();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|_| StoreError::Database)?;
    result
}

pub(super) fn create_chat_schema(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            owner_node_id TEXT NOT NULL,
            title TEXT,
            provider TEXT NOT NULL,
            model TEXT NOT NULL,
            max_tokens INTEGER,
            revision INTEGER NOT NULL DEFAULT 0,
            last_event_sequence INTEGER NOT NULL DEFAULT 0,
            created_order INTEGER NOT NULL UNIQUE
        );
        CREATE TABLE turns (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            user_message_id TEXT NOT NULL,
            assistant_message_id TEXT,
            status TEXT NOT NULL,
            requested_model TEXT NOT NULL,
            resolved_model TEXT,
            input_tokens TEXT,
            output_tokens TEXT,
            total_tokens TEXT,
            total_source TEXT,
            error_code TEXT,
            error_message TEXT,
            cancellation_requested INTEGER NOT NULL DEFAULT 0,
            UNIQUE(session_id, ordinal)
        );
        CREATE INDEX turns_status ON turns(status);
        CREATE TABLE messages (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            role TEXT NOT NULL,
            text TEXT NOT NULL,
            status TEXT NOT NULL,
            UNIQUE(session_id, ordinal)
        );
        CREATE INDEX messages_session_order ON messages(session_id, ordinal);
        CREATE TABLE session_events (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            sequence INTEGER NOT NULL,
            kind TEXT NOT NULL,
            turn_id TEXT,
            message_id TEXT,
            revision INTEGER NOT NULL,
            PRIMARY KEY(session_id, sequence)
        );
        CREATE TABLE commands (
            command_id TEXT PRIMARY KEY,
            scope TEXT NOT NULL,
            payload TEXT NOT NULL,
            receipt TEXT NOT NULL
        );
        CREATE TABLE session_order (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            next_order INTEGER NOT NULL
        );
        INSERT INTO session_order(singleton,next_order) VALUES(1,1);",
    )
    .map_err(|_| StoreError::Database)
}

pub(super) fn steering_migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    tx.execute_batch(
        "ALTER TABLE turns ADD COLUMN immediate_requested INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN pause_requested INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN usage_unknown INTEGER NOT NULL DEFAULT 0;
         ALTER TABLE turns ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;
         CREATE TABLE steering_instructions (
            id TEXT PRIMARY KEY,
            command_id TEXT NOT NULL UNIQUE,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
            text TEXT NOT NULL,
            delivery TEXT NOT NULL CHECK(delivery IN ('next-boundary','immediate')),
            status TEXT NOT NULL CHECK(status IN ('accepted','applied','rejected')),
            error_code TEXT,
            created_order INTEGER NOT NULL UNIQUE
         );
         CREATE INDEX steering_turn_status ON steering_instructions(turn_id,status,created_order);",
    )
    .map_err(|_| StoreError::Database)
}

pub(super) const MAX_PENDING_STEERING_PER_TURN: i64 = 32;

pub(super) fn default_node_name() -> String {
    env::var("COMPUTERNAME")
        .or_else(|_| env::var("HOSTNAME"))
        .ok()
        .filter(|name| valid_node_name(name))
        .unwrap_or_else(|| "Hive node".to_owned())
}

pub(super) fn valid_node_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_NODE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

pub(super) fn random_node_id() -> StoreResult<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| StoreError::Unavailable)?;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut output, "{byte:02x}").map_err(|_| StoreError::Unavailable)?;
    }
    Ok(output)
}

pub(super) fn query_node(connection: &Connection) -> StoreResult<NodeResponse> {
    connection
        .query_row(
            "SELECT node_id, name, os FROM node_identity WHERE singleton = 1",
            [],
            |row| {
                Ok(NodeResponse {
                    node_id: row.get(0)?,
                    name: row.get(1)?,
                    os: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|_| StoreError::Database)?
        .ok_or(StoreError::Database)
        .and_then(|identity| {
            validate_identity(&identity)?;
            Ok(identity)
        })
}

pub(super) fn validate_identity(identity: &NodeResponse) -> StoreResult<()> {
    if identity.node_id.len() != 64
        || !identity
            .node_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !valid_node_name(&identity.name)
        || identity.os.trim().is_empty()
        || identity.os.len() > MAX_NODE_NAME_BYTES
        || identity.os.chars().any(char::is_control)
    {
        return Err(StoreError::Database);
    }
    Ok(())
}

pub(super) fn verify_pragmas(connection: &Connection, busy_timeout: Duration) -> StoreResult<()> {
    let journal: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let busy: i64 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(|_| StoreError::Database)?;
    let expected_busy = busy_timeout.as_millis().min(i64::MAX as u128) as i64;
    if !journal.eq_ignore_ascii_case("wal")
        || synchronous != 2
        || foreign_keys != 1
        || busy != expected_busy
    {
        return Err(StoreError::Database);
    }
    Ok(())
}
