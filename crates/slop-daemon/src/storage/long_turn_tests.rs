use super::tests::{create_request, open_test};
use super::*;
use tempfile::TempDir;

#[tokio::test]
async fn default_workspace_can_pass_old_step_and_message_limits_with_a_queued_followup() {
    let data = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let store = open_test(&data, None).await;
    let client = store.client();
    let mut request = create_request("long-create", None);
    request.workspace = super::tests::select_workspace(
        &client,
        "long",
        serde_json::from_value(serde_json::json!({
            "root":workspace.path().to_string_lossy(),"allowed_tools":["read"]
        }))
        .unwrap(),
    )
    .await;
    let session = client.create_session(request).await.unwrap().session_id;
    let first = client
        .send_message(
            &session,
            SendMessageRequest {
                command_id: "long-first".into(),
                text: "keep working".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let second = client
        .send_message(
            &session,
            SendMessageRequest {
                command_id: "long-second".into(),
                delivery: Some(DeliveryMode::AfterFinish),
                text: "queued followup".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.max_model_requests, u32::MAX);
    assert_eq!(work.execution.as_ref().unwrap().max_tool_calls, u32::MAX);
    let turn = first.turn_id.unwrap();
    let insert_turn = turn.clone();
    let insert_session = session.clone();
    client.submit(move |db| {
        let tx = db.unchecked_transaction().map_err(|_| StoreError::Database)?;
        for index in 0..1100 {
            let ordinal = execution::next_message_ordinal(&tx, &insert_turn)?;
            tx.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'assistant','completed step','completed')",
                rusqlite::params![format!("long-message-{index}"),insert_session,insert_turn,ordinal]).map_err(|_| StoreError::Database)?;
        }
        tx.commit().map_err(|_| StoreError::Database)
    }).await.unwrap();
    client
        .finish_chat_turn(
            &turn,
            slop_runtime::chat::ChatOutcome {
                text: "done".into(),
                resolved_model: None,
                usage: Default::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    let next = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(next.turn_id, second.turn_id.unwrap());
    assert_eq!(next.history.len(), 1103);
    assert_eq!(next.history.first().unwrap().visible_text(), "keep working");
    assert_eq!(
        next.history.last().unwrap().visible_text(),
        "queued followup"
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_thirteen_message_order_upgrade_preserves_frontiers_and_queued_users() {
    let data = TempDir::new().unwrap();
    let store = open_test(&data, None).await;
    let client = store.client();
    let session = client
        .create_session(create_request("migration-create", None))
        .await
        .unwrap()
        .session_id;
    let first = client
        .send_message(
            &session,
            SendMessageRequest {
                command_id: "migration-first".into(),
                text: "first prompt".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client.claim_next_turn().await.unwrap().unwrap();
    let turn = first.turn_id.unwrap();
    client
        .finish_chat_turn(
            &turn,
            slop_runtime::chat::ChatOutcome {
                text: "first reply".into(),
                resolved_model: None,
                usage: Default::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    client
        .send_message(
            &session,
            SendMessageRequest {
                command_id: "migration-next".into(),
                text: "queued prompt".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let original = client.messages(&session, None, 200).await.unwrap().items;
    store.shutdown().await.unwrap();
    let db = Connection::open(data.path().join(DATABASE_FILE)).unwrap();
    workspaces::legacy_fixture(&db).unwrap();
    let tx = db.unchecked_transaction().unwrap();
    tx.execute_batch(&format!("UPDATE messages SET ordinal=-(ordinal/{MESSAGE_ORDER_STRIDE}*1024+ordinal%{MESSAGE_ORDER_STRIDE}); UPDATE messages SET ordinal=-ordinal; DROP TABLE child_notifications; ALTER TABLE child_links DROP COLUMN spawned_at_ms; ALTER TABLE child_links DROP COLUMN detached; ALTER TABLE child_links DROP COLUMN notification_mode; ALTER TABLE child_waits DROP COLUMN result; ALTER TABLE child_waits DROP COLUMN deadline_ms; ALTER TABLE child_waits DROP COLUMN mode; PRAGMA user_version=13;")).unwrap();
    tx.execute("INSERT INTO model_requests(id,turn_id,message_id,ordinal,requested_model,requested_settings,settings,status,context) VALUES('migration-summary',?1,?2,1,'opencode-go/glm-5.3-flash','{}','{}','completed','{\"compaction\":true}')",
        rusqlite::params![turn,original[1].id]).unwrap();
    tx.execute("INSERT INTO session_context(session_id,through_ordinal,messages,request_id) VALUES(?1,1025,'[]','migration-summary')",[&session]).unwrap();
    tx.commit().unwrap();
    drop(db);
    let store = open_test(&data, None).await;
    let client = store.client();
    assert_eq!(
        client.messages(&session, None, 200).await.unwrap().items,
        original
    );
    let query_session = session.clone();
    let through = client
        .submit(move |db| {
            db.query_row(
                "SELECT through_ordinal FROM session_context WHERE session_id=?1",
                [query_session],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|_| StoreError::Database)
        })
        .await
        .unwrap();
    assert_eq!(through, MESSAGE_ORDER_STRIDE + 1);
    assert_eq!(
        client.messages(&session, None, 1).await.unwrap().next_after,
        Some(MESSAGE_ORDER_STRIDE as u64)
    );
    store.shutdown().await.unwrap();
    let store = open_test(&data, None).await;
    assert_eq!(
        store
            .client()
            .messages(&session, None, 200)
            .await
            .unwrap()
            .items,
        original
    );
    store.shutdown().await.unwrap();
}
