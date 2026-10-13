use super::*;
use tempfile::TempDir;

async fn open_test(dir: &TempDir, name: Option<&str>) -> Store {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
            .expect("private temp dir");
    }
    Store::open(
        dir.path().to_path_buf(),
        name.map(str::to_owned),
        8,
        Duration::from_millis(200),
    )
    .await
    .expect("store opens")
}

fn create_request(command_id: &str, title: Option<&str>) -> CreateSessionRequest {
    CreateSessionRequest {
        command_id: command_id.to_owned(),
        title: title.map(str::to_owned),
        provider: "opencode-go".to_owned(),
        model: "glm-5.3-flash".to_owned(),
        max_tokens: Some(1024),

        ..Default::default()
    }
}

#[tokio::test]
async fn inbox_changes_are_durable_idempotent_and_do_not_cancel_work() {
    use slop_protocol::chat::UpdateSessionInboxRequest;
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let mut create = create_request("inbox-create", Some("Inbox regression"));
    create.prompt = Some("Queued work must survive trash".into());
    let created = client.create_session(create).await.unwrap();
    let id = &created.session_id;
    let session = client.session(id).await.unwrap();
    assert!(session.inbox.is_none());
    let request = UpdateSessionInboxRequest {
        command_id: "inbox-trash".into(),
        starred: Some(true),
        trashed: Some(true),
        read_sequence: Some(session.last_event_sequence),
        ..Default::default()
    };
    let receipt = client
        .update_session_inbox(id, request.clone())
        .await
        .unwrap();
    assert_eq!(
        client
            .update_session_inbox(id, request.clone())
            .await
            .unwrap(),
        receipt
    );
    let state = client.session(id).await.unwrap();
    assert!(state.inbox.as_ref().unwrap().trashed);
    assert_eq!(
        state.inbox.as_ref().unwrap().read_sequence,
        state.last_event_sequence
    );
    assert_eq!(
        client
            .turn(created.turn_id.as_ref().unwrap())
            .await
            .unwrap()
            .status,
        "queued"
    );
    let mut conflict = request;
    conflict.trashed = Some(false);
    assert!(matches!(
        client.update_session_inbox(id, conflict).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        client
            .update_session_inbox(
                id,
                UpdateSessionInboxRequest {
                    command_id: "inbox-future-read".into(),
                    read_sequence: Some(state.last_event_sequence + 1),
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::Invalid)
    ));
    client
        .update_session_inbox(
            id,
            UpdateSessionInboxRequest {
                command_id: "inbox-stale-import".into(),
                starred: Some(false),
                trashed: Some(false),
                only_if_unset: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(client.session(id).await.unwrap().inbox.unwrap().trashed);
    store.shutdown().await.unwrap();
    let reopened = open_test(&dir, None).await;
    let client = reopened.client();
    let state = client
        .sessions(None, 10)
        .await
        .unwrap()
        .items
        .pop()
        .unwrap();
    assert!(state.inbox.unwrap().starred);
    client
        .update_session_inbox(
            id,
            UpdateSessionInboxRequest {
                command_id: "inbox-restore".into(),
                archived: Some(false),
                trashed: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client
        .update_session_inbox(
            id,
            UpdateSessionInboxRequest {
                command_id: "inbox-archive".into(),
                archived: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let state = client.session(id).await.unwrap();
    let inbox = state.inbox.unwrap();
    assert!(!inbox.trashed && inbox.archived && inbox.starred);
    assert_eq!(inbox.read_sequence, state.last_event_sequence);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_nine_upgrade_preserves_conversations_history_and_receipts() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let mut request = create_request("schema-nine-chat", Some("Existing conversation"));
    request.prompt = Some("Preserved history".into());
    let receipt = client.create_session(request.clone()).await.unwrap();
    let before = client.session(&receipt.session_id).await.unwrap();
    let history = client
        .messages(&receipt.session_id, None, 10)
        .await
        .unwrap();
    store.shutdown().await.unwrap();
    let connection = Connection::open(dir.path().join(DATABASE_FILE)).unwrap();
    connection.execute_batch("ALTER TABLE sessions DROP COLUMN inbox; DROP TABLE conversation_drafts; DROP TABLE session_context; ALTER TABLE model_requests DROP COLUMN context; ALTER TABLE messages DROP COLUMN provider; ALTER TABLE messages DROP COLUMN model; PRAGMA user_version=9;").unwrap();
    drop(connection);
    let reopened = open_test(&dir, None).await;
    let client = reopened.client();
    assert_eq!(client.session(&receipt.session_id).await.unwrap(), before);
    assert_eq!(
        client
            .messages(&receipt.session_id, None, 10)
            .await
            .unwrap(),
        history
    );
    assert_eq!(client.create_session(request).await.unwrap(), receipt);
    assert!(client.drafts(None, 10).await.unwrap().items.is_empty());
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn drafts_survive_restart_and_are_consumed_atomically_with_a_send() {
    use slop_protocol::chat::{ConversationDraft, SaveConversationDraftRequest};
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let draft = ConversationDraft {
        id: "draft-one".into(),
        text: "Saved instructions".into(),
        ..Default::default()
    };
    client
        .save_draft(
            &draft.id,
            SaveConversationDraftRequest {
                draft: draft.clone(),
                only_if_absent: false,
            },
        )
        .await
        .unwrap();
    let stale = ConversationDraft {
        text: "Stale browser copy".into(),
        ..draft.clone()
    };
    assert_eq!(
        client
            .save_draft(
                &draft.id,
                SaveConversationDraftRequest {
                    draft: stale,
                    only_if_absent: true
                }
            )
            .await
            .unwrap(),
        draft
    );
    let invalid = ConversationDraft {
        text: "x".repeat(MAX_MESSAGE_BYTES + 1),
        ..draft.clone()
    };
    assert!(matches!(
        client
            .save_draft(
                &draft.id,
                SaveConversationDraftRequest {
                    draft: invalid,
                    only_if_absent: false
                }
            )
            .await,
        Err(StoreError::Invalid)
    ));
    store.shutdown().await.unwrap();
    let reopened = open_test(&dir, None).await;
    let client = reopened.client();
    assert_eq!(
        client.drafts(None, 10).await.unwrap().items,
        vec![draft.clone()]
    );
    let mut create = create_request("draft-send", None);
    create.draft_id = Some(draft.id.clone());
    create.prompt = Some("".into());
    assert!(client.create_session(create.clone()).await.is_err());
    assert_eq!(
        client.drafts(None, 10).await.unwrap().items,
        vec![draft.clone()]
    );
    create.prompt = Some(draft.text.clone());
    let accepted = client.create_session(create.clone()).await.unwrap();
    assert!(client.drafts(None, 10).await.unwrap().items.is_empty());
    assert_eq!(client.create_session(create).await.unwrap(), accepted);
    assert_eq!(client.sessions(None, 10).await.unwrap().items.len(), 1);
    client.remove_draft(&draft.id).await.unwrap();
    client.remove_draft(&draft.id).await.unwrap();
    reopened.shutdown().await.unwrap();
}

#[test]
fn provider_defaults_preserve_settings_object_omission_and_subscription_no_cap() {
    let mut go = create_request("go-default", None);
    go.max_tokens = None;
    go.settings = Some(slop_protocol::execution::GenerationSettings::default());
    assert_eq!(
        execution::session_settings(&go, Some(4096))
            .unwrap()
            .max_output_tokens,
        Some(4096)
    );

    let mut codex = go;
    codex.provider = "codex".into();
    codex.model = "gpt-6.1-sol".into();
    assert_eq!(
        execution::session_settings(&codex, None)
            .unwrap()
            .max_output_tokens,
        None
    );
}

#[test]
fn schema_two_migration_preserves_history_settings_and_command_payloads() {
    let connection = Connection::open_in_memory().unwrap();
    let tx = connection.unchecked_transaction().unwrap();
    create_chat_schema(&tx).unwrap();
    tx.execute("INSERT INTO sessions(id,owner_node_id,provider,model,max_tokens,created_order) VALUES('session','node','opencode-go','glm-5.3-flash',1024,1)",[]).unwrap();
    tx.execute(
        "INSERT INTO commands VALUES('command','scope','{\"text\":\"old payload\"}','{}')",
        [],
    )
    .unwrap();
    for i in 1..=2 {
        tx.execute("INSERT INTO turns(id,session_id,ordinal,user_message_id,assistant_message_id,status,requested_model) VALUES(?1,'session',?2,?3,?4,'completed','opencode-go/glm-5.3-flash')",rusqlite::params![format!("turn-{i}"),i,format!("user-{i}"),format!("assistant-{i}")]).unwrap();
        for (role, offset) in [("user", 0), ("assistant", 1)] {
            tx.execute(
                "INSERT INTO messages VALUES(?1,'session',?2,?3,?4,?5,'completed')",
                rusqlite::params![
                    format!("{role}-{i}"),
                    format!("turn-{i}"),
                    i * 2 + offset,
                    role,
                    format!("{role} {i}")
                ],
            )
            .unwrap();
        }
    }
    tx.pragma_update(None, "user_version", 2).unwrap();
    tx.commit().unwrap();
    migrate(&connection, 2, None).unwrap();
    let history = list_messages(&connection, "session", None, 20).unwrap();
    assert_eq!(
        history
            .items
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["user 1", "assistant 1", "user 2", "assistant 2"]
    );
    assert!(history.items.iter().all(|m| m.blocks.len() == 1));
    assert!(
        history
            .items
            .iter()
            .all(|m| m.provider == "opencode-go" && m.model == "glm-5.3-flash")
    );
    assert_eq!(
        get_session(&connection, "session")
            .unwrap()
            .settings
            .max_output_tokens,
        Some(1024)
    );
    let payload: String = connection
        .query_row(
            "SELECT payload FROM commands WHERE command_id='command'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(payload, "{\"text\":\"old payload\"}");
    let version: i64 = connection
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[tokio::test]
async fn accepted_settings_are_frozen_and_workspace_is_leased_across_sessions() {
    use slop_protocol::execution::{GenerationSettings, WorkspacePolicy};
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let policy = WorkspacePolicy {
        root: workspace.path().to_str().unwrap().into(),
        allowed_tools: vec!["read".into()],
        shell_timeout_ms: 1000,
        max_output_bytes: 1024,
        max_tool_calls: 4,
        max_model_requests: 4,
    };
    let mut first = create_request("lease-first", None);
    first.execution = Some(policy.clone());
    let first = client.create_session(first).await.unwrap();
    let mut second = create_request("lease-second", None);
    second.execution = Some(policy);
    let second = client.create_session(second).await.unwrap();
    let sent = client
        .send_message(
            &first.session_id,
            SendMessageRequest {
                command_id: "selected".into(),
                text: "hello".into(),
                model: Some("opencode-go/glm-5.3".into()),
                settings: Some(GenerationSettings {
                    max_output_tokens: Some(512),
                    reasoning_effort: None,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    client
        .send_message(
            &second.session_id,
            SendMessageRequest {
                command_id: "blocked-workspace".into(),
                text: "wait".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.turn_id, sent.turn_id.clone().unwrap());
    assert_eq!(work.requested_model, "opencode-go/glm-5.3");
    assert_eq!(work.settings.max_output_tokens, Some(512));
    assert_eq!(
        client
            .session(&first.session_id)
            .await
            .unwrap()
            .settings
            .max_output_tokens,
        Some(1024)
    );
    assert!(client.claim_next_turn().await.unwrap().is_none());
    client
        .finish_chat_turn(
            &work.turn_id,
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
    assert_eq!(next.session_id, second.session_id);
    assert_eq!(next.settings.max_output_tokens, Some(1024));
    let before = client.session(&first.session_id).await.unwrap().revision;
    let unsupported = client
        .send_message(
            &first.session_id,
            SendMessageRequest {
                command_id: "effort".into(),
                text: "reject".into(),
                settings: Some(GenerationSettings {
                    max_output_tokens: None,
                    reasoning_effort: Some("high".into()),
                }),
                ..Default::default()
            },
        )
        .await;
    assert!(matches!(
        unsupported,
        Err(StoreError::Unsupported("settings.reasoning_effort"))
    ));
    assert_eq!(
        client.session(&first.session_id).await.unwrap().revision,
        before
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn boundary_steering_is_durable_idempotent_and_applied_before_readmission() {
    use slop_runtime::chat::ChatRepository;
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("steer-create", None))
        .await
        .unwrap();
    let session = client.session(&created.session_id).await.unwrap();
    let original = SendMessageRequest {
        command_id: "steer-original".into(),
        text: "first task".into(),
        ..Default::default()
    };
    let original_receipt = client
        .send_message(&created.session_id, original.clone())
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.turn_id, original_receipt.turn_id.as_deref().unwrap());
    client
        .begin_request(
            &work.turn_id,
            slop_runtime::agent::RequestIntent {
                context: Default::default(),
                id: "steer-model-request".into(),
                message_id: "steer-assistant-message".into(),
                ordinal: 1,
                requested_model: work.requested_model.clone(),
                requested_settings: work.requested_settings.clone(),
                settings: work.settings.clone(),
            },
        )
        .await
        .unwrap();

    let steering = SendMessageRequest {
        command_id: "steer-boundary".into(),
        text: "change direction".into(),
        delivery: Some(DeliveryMode::NextBoundary),
        ..Default::default()
    };
    let accepted = client
        .send_message_preflighted(
            &created.session_id,
            steering.clone(),
            session.settings.clone(),
            Some(("unsupported_capability", "idle fallback rejected")),
        )
        .await
        .unwrap();
    assert_eq!(accepted.turn_id, original_receipt.turn_id);
    let replay = client
        .send_message(&created.session_id, steering)
        .await
        .unwrap();
    assert_eq!(replay, accepted);
    assert_eq!(
        client
            .steering_instructions(&created.session_id, None, 10)
            .await
            .unwrap()
            .items[0]
            .status,
        "accepted"
    );

    client
        .finish_chat_turn(
            work.turn_id.clone().as_str(),
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: Some(work.requested_model),
                usage: slop_runtime::providers::Usage {
                    input_tokens: Some(13),
                    output_tokens: Some(2),
                    total_tokens: Some(15),
                    total_source: Some(slop_runtime::providers::UsageSource::Reported),
                },
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    let items = client
        .steering_instructions(&created.session_id, None, 10)
        .await
        .unwrap()
        .items;
    assert_eq!(items[0].status, "applied");
    let queued_instruction = client
        .send_message_preflighted(
            &created.session_id,
            SendMessageRequest {
                command_id: "steer-queued-second".into(),
                text: "second direction".into(),
                delivery: Some(DeliveryMode::Immediate),
                ..Default::default()
            },
            session.settings.clone(),
            Some(("unsupported_capability", "idle fallback rejected")),
        )
        .await;
    assert_eq!(
        queued_instruction.unwrap().turn_id.as_deref(),
        Some(work.turn_id.as_str())
    );
    let idle_session = client
        .create_session(create_request("steer-idle-create", None))
        .await
        .unwrap();
    let idle_rejected = client
        .send_message_preflighted(
            &idle_session.session_id,
            SendMessageRequest {
                command_id: "steer-idle-rejected".into(),
                text: "unsupported".into(),
                delivery: Some(DeliveryMode::Immediate),
                ..Default::default()
            },
            client
                .session(&idle_session.session_id)
                .await
                .unwrap()
                .settings,
            Some(("unsupported_capability", "idle fallback rejected")),
        )
        .await;
    assert!(matches!(
        idle_rejected,
        Err(StoreError::Preflight("unsupported_capability", _))
    ));
    let resumed = client.claim_next_turn().await.unwrap().unwrap();
    assert!(
        resumed
            .messages
            .iter()
            .any(|message| message.text == "change direction")
    );
    assert_eq!(resumed.turn_id, work.turn_id);
    assert_eq!(resumed.initial_usage.input_tokens, Some(13));
    assert_eq!(resumed.initial_usage.output_tokens, Some(2));
    assert_eq!(resumed.initial_usage.total_tokens, Some(15));
    client
        .finish_chat_turn(
            &resumed.turn_id,
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: Some(resumed.requested_model.clone()),
                usage: resumed.initial_usage,
                status: slop_runtime::chat::ChatStatus::Interrupted,
                error_code: Some("steering_resume".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();
    let resumed_again = client.claim_next_turn().await.unwrap().unwrap();
    assert!(
        resumed_again
            .messages
            .iter()
            .any(|message| message.text == "change direction")
    );
    assert!(
        resumed_again
            .messages
            .iter()
            .any(|message| message.text == "second direction")
    );
}

#[tokio::test]
async fn pause_resume_is_durable_and_keeps_the_workspace_request_cap() {
    use slop_protocol::execution::WorkspacePolicy;
    let dir = TempDir::new().unwrap();
    let workspace = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let mut request = create_request("pause-create", None);
    request.execution = Some(WorkspacePolicy {
        root: workspace.path().to_string_lossy().into_owned(),
        allowed_tools: vec!["read".into()],
        shell_timeout_ms: 1000,
        max_output_bytes: 1024,
        max_tool_calls: 4,
        max_model_requests: 2,
    });
    let created = client.create_session(request).await.unwrap();
    let receipt = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "pause-turn-message".into(),
                text: "do work".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    client
        .pause_turn(
            work.turn_id.as_str(),
            TurnControlRequest {
                command_id: "pause-command".into(),
            },
        )
        .await
        .unwrap();
    client
        .finish_chat_turn(
            &work.turn_id,
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: Some(work.requested_model.clone()),
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Interrupted,
                error_code: Some("pause_turn".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(client.turn(&work.turn_id).await.unwrap().status, "paused");
    assert!(client.claim_next_turn().await.unwrap().is_none());
    let paused_instruction = client
        .send_message_preflighted(
            &created.session_id,
            SendMessageRequest {
                command_id: "paused-steering".into(),
                text: "resume with this".into(),
                delivery: Some(DeliveryMode::NextBoundary),
                ..Default::default()
            },
            client.session(&created.session_id).await.unwrap().settings,
            Some(("unsupported_capability", "unused while paused")),
        )
        .await
        .unwrap();
    assert_eq!(
        paused_instruction.turn_id.as_deref(),
        Some(work.turn_id.as_str())
    );
    client
        .resume_turn(
            &work.turn_id,
            TurnControlRequest {
                command_id: "resume-command".into(),
            },
        )
        .await
        .unwrap();
    let resumed = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(resumed.turn_id, receipt.turn_id.unwrap());
    assert_eq!(resumed.max_model_requests, 2);
    assert!(
        resumed
            .messages
            .iter()
            .any(|message| message.text == "resume with this")
    );
}

#[tokio::test]
async fn canceling_paused_continuation_rejects_accepted_instructions() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("cancel-paused-create", None))
        .await
        .unwrap();
    let receipt = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "cancel-paused-turn".into(),
                text: "start".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    client
        .pause_turn(
            &work.turn_id,
            TurnControlRequest {
                command_id: "cancel-paused-pause".into(),
            },
        )
        .await
        .unwrap();
    client
        .finish_chat_turn(
            &work.turn_id,
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: Some(work.requested_model),
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Interrupted,
                error_code: Some("pause_turn".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();
    client
        .send_message_preflighted(
            &created.session_id,
            SendMessageRequest {
                command_id: "cancel-paused-instruction".into(),
                text: "not applied".into(),
                delivery: Some(DeliveryMode::Immediate),
                ..Default::default()
            },
            client.session(&created.session_id).await.unwrap().settings,
            None,
        )
        .await
        .unwrap();
    client
        .cancel_turn(
            &work.turn_id,
            CancelTurnRequest {
                command_id: "cancel-paused-cmd".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        client.turn(&work.turn_id).await.unwrap().status,
        "cancelled"
    );
    let instructions = client
        .steering_instructions(&created.session_id, None, 10)
        .await
        .unwrap()
        .items;
    assert_eq!(instructions[0].status, "rejected");
    assert_eq!(
        instructions[0].error_code.as_deref(),
        Some("turn_cancelled")
    );
    assert_eq!(receipt.turn_id.as_deref(), Some(work.turn_id.as_str()));
}

#[tokio::test]
async fn immediate_unknown_usage_resumes_without_replaying_or_fabricating_totals() {
    use slop_runtime::chat::ChatRepository;
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("unknown-usage-create", None))
        .await
        .unwrap();
    let receipt = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "unknown-usage-turn".into(),
                text: "start".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    let request_id = "unknown-running-request";
    client
        .begin_request(
            &work.turn_id,
            slop_runtime::agent::RequestIntent {
                context: Default::default(),
                id: request_id.into(),
                message_id: "unknown-running-message".into(),
                ordinal: 1,
                requested_model: work.requested_model.clone(),
                requested_settings: work.requested_settings.clone(),
                settings: work.settings.clone(),
            },
        )
        .await
        .unwrap();
    client
        .send_message_preflighted(
            &created.session_id,
            SendMessageRequest {
                command_id: "unknown-immediate".into(),
                text: "rethink".into(),
                delivery: Some(DeliveryMode::Immediate),
                ..Default::default()
            },
            client.session(&created.session_id).await.unwrap().settings,
            None,
        )
        .await
        .unwrap();
    client
        .fail_request(&work.turn_id, request_id, "steering_interrupted")
        .await
        .unwrap();
    client
        .finish_chat_turn(
            &work.turn_id,
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: None,
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Interrupted,
                error_code: Some("steering_resume_unknown".into()),
                error_message: None,
            },
        )
        .await
        .unwrap();
    let requests = client
        .model_requests(&work.turn_id, None, 10)
        .await
        .unwrap()
        .items;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].status, "interrupted");
    assert_eq!(requests[0].usage, None);
    let resumed = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(resumed.turn_id, receipt.turn_id.unwrap());
    assert_eq!(resumed.next_request_ordinal, 2);
    assert_eq!(resumed.initial_usage.input_tokens, None);
    assert_eq!(resumed.initial_usage.output_tokens, None);
    assert_eq!(resumed.initial_usage.total_tokens, None);
    assert!(
        resumed
            .messages
            .iter()
            .any(|message| message.text == "rethink")
    );
}

#[tokio::test]
async fn restart_pairs_unfinished_tools_without_replaying_completed_effects() {
    use slop_runtime::{
        agent::{RequestCompletion, RequestIntent, ToolIntent},
        chat::ChatRepository,
        providers::{Usage, inference::*},
        tools::ToolOutcome,
    };
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let session = client
        .create_session(create_request("tool-recovery", None))
        .await
        .unwrap();
    let turn = client
        .send_message(
            &session.session_id,
            SendMessageRequest {
                command_id: "tool-turn".into(),
                text: "do work".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .turn_id
        .unwrap();
    client.claim_next_turn().await.unwrap().unwrap();
    client
        .begin_request(
            &turn,
            RequestIntent {
                context: Default::default(),
                id: "request".into(),
                message_id: "assistant".into(),
                ordinal: 1,
                requested_model: "opencode-go/glm-5.3-flash".into(),
                requested_settings: Default::default(),
                settings: GenerationSettings {
                    max_output_tokens: Some(1024),
                    reasoning_effort: None,
                },
            },
        )
        .await
        .unwrap();
    let intents: Vec<_> = (0..3)
        .map(|i| ToolIntent {
            id: format!("invocation-{i}"),
            request_id: "request".into(),
            result_message_id: format!("result-{i}"),
            call_id: format!("call-{i}"),
            provider_call_id: format!("provider-{i}"),
            name: "read".into(),
            arguments: serde_json::json!({"path":"file"}),
        })
        .collect();
    let blocks = intents
        .iter()
        .enumerate()
        .map(|(i, intent)| ContentBlock {
            id: format!("block-{i}"),
            content: BlockContent::ToolCall {
                call_id: intent.call_id.clone(),
                provider_call_id: intent.provider_call_id.clone(),
                name: intent.name.clone(),
                arguments: intent.arguments.clone(),
            },
        })
        .collect();
    client.complete_request(&turn, RequestCompletion {
                compaction: None, request_id: "request".into(),
            response: InferenceResponse { resolved_model: Some("vendor/resolved-tool-model".into()), usage: Usage::default(), finish_reason: FinishReason::ToolCalls,
                message: InferenceMessage { role: MessageRole::Assistant, blocks, continuation: Some(Continuation {
                    scope:"scope".into(), provider:"opencode-go".into(), model:"glm-5.3-flash".into(), wire:"openai-chat-completions".into(), required:true,
                    items:vec![serde_json::json!({"reasoning_content":"private test continuation"})],
                }) },
            }, tools: intents.clone(),
        }).await.unwrap();
    assert_eq!(
        client
            .tool_invocations(&turn, None, 10)
            .await
            .unwrap()
            .items
            .len(),
        3
    );
    client.start_tool(&turn, &intents[0]).await.unwrap();
    let outcome = ToolOutcome {
        status: "completed".into(),
        output: "known result".into(),
        artifacts: vec![],
        error_code: None,
        effects_unknown: false,
    };
    client
        .finish_tool(&turn, &intents[0], outcome.clone())
        .await
        .unwrap();
    client
        .finish_tool(&turn, &intents[0], outcome)
        .await
        .unwrap(); // receipt retry, never execution
    client.start_tool(&turn, &intents[1]).await.unwrap();
    store.shutdown().await.unwrap();
    let connection = Connection::open(dir.path().join(DATABASE_FILE)).unwrap();
    connection.execute_batch("ALTER TABLE messages DROP COLUMN provider; ALTER TABLE messages DROP COLUMN model; PRAGMA user_version=11;").unwrap();
    drop(connection);
    let store = open_test(&dir, None).await;
    let client = store.client();
    let tools = client
        .tool_invocations(&turn, None, 10)
        .await
        .unwrap()
        .items;
    assert_eq!(tools[0].status, "completed");
    assert!(!tools[0].effects_unknown);
    assert_eq!(tools[1].error_code.as_deref(), Some("daemon_restarted"));
    assert!(tools[1].effects_unknown);
    assert!(!tools[2].effects_unknown);
    assert_eq!(client.turn(&turn).await.unwrap().status, "interrupted");
    let (has_non_text, required) = client
        .context_requirements(&session.session_id)
        .await
        .unwrap();
    assert!(has_non_text);
    assert_eq!(
        required,
        vec![(
            "opencode-go".into(),
            "glm-5.3-flash".into(),
            "openai-chat-completions".into(),
        )]
    );
    let history = client
        .messages(&session.session_id, None, 20)
        .await
        .unwrap();
    assert_eq!(history.items.len(), 5); // user, assistant, three paired results
    assert_eq!(history.items[0].provider, "opencode-go");
    assert_eq!(history.items[0].model, "glm-5.3-flash");
    for message in &history.items[1..] {
        assert_eq!(message.provider, "opencode-go");
        assert_eq!(message.model, "vendor/resolved-tool-model");
        assert_eq!(client.message(&message.id).await.unwrap(), *message);
    }
    let public = serde_json::to_string(&history).unwrap();
    assert!(!public.contains("private test continuation"));
    assert!(!public.contains("provider-0"));
    assert!(client.claim_next_turn().await.unwrap().is_none());
    client
        .send_message(
            &session.session_id,
            SendMessageRequest {
                command_id: "inspect".into(),
                text: "inspect results".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let next = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(next.history.len(), 6);
    assert!(next.history.iter().any(|m| m.continuation.is_some()));
    let events = client.events(&session.session_id, 0, 200).await.unwrap();
    let count = events
        .items
        .iter()
        .filter(|e| e.kind == "tool_failed")
        .count();
    assert_eq!(count, 2);
    store.shutdown().await.unwrap();
    let store = open_test(&dir, None).await;
    let events = store
        .client()
        .events(&session.session_id, 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .items
            .iter()
            .filter(|e| e.kind == "tool_failed")
            .count(),
        count
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn identity_persists_and_name_override_is_transactional() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, Some("first")).await;
    let first = store.client().node().await.expect("identity");
    assert_eq!(first.name, "first");
    store.shutdown().await.expect("shutdown");

    let store = open_test(&dir, None).await;
    let persisted = store.client().node().await.expect("identity");
    assert_eq!(persisted.node_id, first.node_id);
    assert_eq!(persisted.name, "first");
    store.shutdown().await.expect("shutdown");

    let store = open_test(&dir, Some("second")).await;
    let changed = store.client().node().await.expect("identity");
    assert_eq!(changed.node_id, first.node_id);
    assert_eq!(changed.name, "second");
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn chat_commands_are_atomic_and_idempotent_with_payload_conflicts_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("create-1", Some("first")))
        .await
        .expect("create");
    let retry = client
        .create_session(create_request("create-1", Some("first")))
        .await
        .expect("deduplicated create");
    assert_eq!(created, retry);
    let conflict = client
        .create_session(create_request("create-1", Some("different")))
        .await;
    assert!(matches!(conflict, Err(StoreError::Conflict)));
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-1".into(),
                text: "hello".into(),
                expected_revision: Some(created.revision),

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let repeated = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-1".into(),
                text: "hello".into(),
                expected_revision: Some(created.revision),

                ..Default::default()
            },
        )
        .await
        .expect("deduplicated send");
    assert_eq!(sent, repeated);
    assert!(matches!(
        client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "send-1".into(),
                    text: "changed".into(),
                    expected_revision: Some(created.revision),
                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        client
            .messages(&created.session_id, None, 50)
            .await
            .expect("messages")
            .items
            .len(),
        1
    );
    let events = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events");
    assert_eq!(events.items.len(), 2);
    assert_eq!(events.items[1].sequence, sent.event_sequence);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn claim_context_is_ordered_and_excludes_later_queued_users() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("create-2", None))
        .await
        .expect("create");
    let first = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-2a".into(),
                text: "first question".into(),
                expected_revision: Some(created.revision),

                ..Default::default()
            },
        )
        .await
        .expect("first");
    let second = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-2b".into(),
                text: "later queued question".into(),
                expected_revision: Some(first.revision),

                ..Default::default()
            },
        )
        .await
        .expect("second");
    let work = client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("turn");
    assert_eq!(work.turn_id, first.turn_id.clone().expect("turn id"));
    assert_eq!(work.requested_model, "opencode-go/glm-5.3-flash");
    assert_eq!(work.max_tokens, Some(1024));
    assert_eq!(work.messages.len(), 1);
    assert_eq!(work.messages[0].text, "first question");
    assert!(
        client
            .claim_next_turn()
            .await
            .expect("claim while active")
            .is_none()
    );
    client
        .finish_chat_turn(
            &work.turn_id,
            slop_runtime::chat::ChatOutcome {
                text: "answer".into(),
                resolved_model: Some(work.requested_model.clone()),
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .expect("finish");
    let next = client
        .claim_next_turn()
        .await
        .expect("next claim")
        .expect("second turn");
    assert_eq!(next.turn_id, second.turn_id.expect("turn id"));
    let context: Vec<String> = next
        .messages
        .into_iter()
        .map(|message| message.text)
        .collect();
    assert_eq!(
        context,
        ["first question", "answer", "later queued question"]
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn completed_request_usage_is_durable_before_the_turn_finishes() {
    use slop_runtime::{
        agent::{RequestCompletion, RequestIntent},
        chat::ChatRepository,
        providers::{
            Usage,
            inference::{FinishReason, InferenceMessage, InferenceResponse, MessageRole},
        },
    };
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let session = client
        .create_session(create_request("usage-checkpoint-create", None))
        .await
        .unwrap()
        .session_id;
    let receipt = client
        .send_message(
            &session,
            SendMessageRequest {
                command_id: "usage-checkpoint-send".into(),
                text: "goal".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    let turn = receipt.turn_id.unwrap();
    client
        .begin_request(
            &turn,
            RequestIntent {
                id: "usage-checkpoint-request".into(),
                message_id: "usage-checkpoint-message".into(),
                ordinal: 1,
                requested_model: work.requested_model.clone(),
                requested_settings: work.requested_settings.clone(),
                settings: work.settings.clone(),
                context: slop_runtime::agent::context::RequestContext {
                    input_bytes: 1024,
                    compaction: false,
                },
            },
        )
        .await
        .unwrap();
    client
        .complete_request(
            &turn,
            RequestCompletion {
                request_id: "usage-checkpoint-request".into(),
                tools: Vec::new(),
                compaction: None,
                response: InferenceResponse {
                    resolved_model: Some("reported-model-build".into()),
                    usage: Usage::from_reported(Some(30000), Some(10), Some(30010)),
                    finish_reason: FinishReason::Stop,
                    message: InferenceMessage::text(
                        MessageRole::Assistant,
                        "usage-block".into(),
                        "completed step".into(),
                    ),
                },
            },
        )
        .await
        .unwrap();
    assert_eq!(
        client
            .turn(&turn)
            .await
            .unwrap()
            .usage
            .unwrap()
            .total_tokens,
        Some(30010)
    );
    let reply = client.message("usage-checkpoint-message").await.unwrap();
    assert_eq!(reply.provider, "opencode-go");
    assert_eq!(reply.model, "reported-model-build");
    client
        .begin_request(
            &turn,
            RequestIntent {
                id: "second-checkpoint-request".into(),
                message_id: "second-checkpoint-message".into(),
                ordinal: 2,
                requested_model: work.requested_model,
                requested_settings: work.requested_settings,
                settings: work.settings,
                context: Default::default(),
            },
        )
        .await
        .unwrap();
    client
        .complete_request(
            &turn,
            RequestCompletion {
                request_id: "second-checkpoint-request".into(),
                tools: Vec::new(),
                compaction: None,
                response: InferenceResponse {
                    resolved_model: Some("second-reported-build".into()),
                    usage: Usage::from_reported(Some(5), Some(2), Some(7)),
                    finish_reason: FinishReason::Stop,
                    message: InferenceMessage::text(
                        MessageRole::Assistant,
                        "second-usage-block".into(),
                        "second completed step".into(),
                    ),
                },
            },
        )
        .await
        .unwrap();
    let second_reply = client.message("second-checkpoint-message").await.unwrap();
    assert_eq!(second_reply.model, "second-reported-build");
    assert_eq!(client.message(&reply.id).await.unwrap(), reply);
    store.shutdown().await.unwrap();
    let connection = Connection::open(dir.path().join(DATABASE_FILE)).unwrap();
    connection.execute_batch("ALTER TABLE messages DROP COLUMN provider; ALTER TABLE messages DROP COLUMN model; PRAGMA user_version=11;").unwrap();
    drop(connection);
    let reopened = open_test(&dir, None).await;
    let recovered = reopened.client().turn(&turn).await.unwrap();
    assert_eq!(recovered.status, "interrupted");
    assert_eq!(recovered.usage.unwrap().total_tokens, Some(30017));
    assert_eq!(reopened.client().message(&reply.id).await.unwrap(), reply);
    assert_eq!(
        reopened.client().message(&second_reply.id).await.unwrap(),
        second_reply
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn messages_keep_turn_models_across_provider_switches_and_migration() {
    use slop_runtime::{
        chat::{ChatOutcome, ChatStatus},
        providers::Usage,
    };
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let client = store.client();
    let session = client
        .create_session(create_request("message-models-create", None))
        .await
        .unwrap()
        .session_id;
    let selections = [
        (
            None,
            "opencode-go",
            "glm-5.3-flash",
            Some("go-reported-build"),
        ),
        (
            Some("opencode-zen/glm-5.3"),
            "opencode-zen",
            "glm-5.3",
            Some("vendor/zen-reported-build"),
        ),
        (Some("codex/gpt-6.1-sol"), "codex", "gpt-6.1-sol", None),
        (None, "opencode-go", "glm-5.3-flash", None),
    ];
    for (index, (selection, provider, model, reported)) in selections.iter().enumerate() {
        let request = SendMessageRequest {
            command_id: format!("message-models-{index}"),
            text: format!("question {index}"),
            model: selection.map(str::to_owned),
            ..Default::default()
        };
        let receipt = client
            .send_message(&session, request.clone())
            .await
            .unwrap();
        let work = client.claim_next_turn().await.unwrap().unwrap();
        assert_eq!(work.requested_model, format!("{provider}/{model}"));
        let user = client
            .message(receipt.message_id.as_ref().unwrap())
            .await
            .unwrap();
        assert_eq!(user.provider, *provider);
        assert_eq!(user.model, *model);
        client
            .checkpoint_visible(&work.turn_id, "partial reply")
            .await
            .unwrap();
        let partial = client
            .message(
                client
                    .turn(&work.turn_id)
                    .await
                    .unwrap()
                    .assistant_message_id
                    .as_ref()
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(partial.provider, *provider);
        assert_eq!(partial.model, *model);
        client
            .finish_chat_turn(
                &work.turn_id,
                ChatOutcome {
                    status: ChatStatus::Completed,
                    text: format!("answer {index}"),
                    resolved_model: reported.map(str::to_owned),
                    usage: Usage::default(),
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .unwrap();
        let reply = client.message(&partial.id).await.unwrap();
        assert_eq!(reply.provider, *provider);
        assert_eq!(reply.model, reported.unwrap_or(model));
        assert_eq!(
            client.send_message(&session, request).await.unwrap(),
            receipt
        );
        assert_eq!(client.message(&user.id).await.unwrap(), user);
    }
    let history = client.messages(&session, None, 20).await.unwrap();
    assert_eq!(history.items.len(), 8);
    let mut cursor = None;
    let mut paged = Vec::new();
    loop {
        let page = client.messages(&session, cursor, 1).await.unwrap();
        paged.extend(page.items);
        cursor = page.next_after;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(paged, history.items);
    store.shutdown().await.unwrap();
    // Exercise the previous schema's backfill using each turn's frozen model,
    // including resolved aliases and turns that override the session provider.
    let connection = Connection::open(dir.path().join(DATABASE_FILE)).unwrap();
    connection.execute_batch("ALTER TABLE messages DROP COLUMN provider; ALTER TABLE messages DROP COLUMN model; PRAGMA user_version=11;").unwrap();
    drop(connection);
    let reopened = open_test(&dir, None).await;
    assert_eq!(
        reopened
            .client()
            .messages(&session, None, 20)
            .await
            .unwrap(),
        history
    );
    for item in &history.items {
        assert_eq!(reopened.client().message(&item.id).await.unwrap(), *item);
    }
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn oversized_context_is_claimed_as_a_bounded_compaction_page() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("context-create", None))
        .await
        .expect("create");
    for index in 0..128 {
        let sent = client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: format!("context-{index}"),
                    text: "question".into(),
                    expected_revision: None,

                    ..Default::default()
                },
            )
            .await
            .expect("send");
        let work = client
            .claim_next_turn()
            .await
            .expect("claim")
            .expect("work");
        assert_eq!(work.turn_id, sent.turn_id.expect("turn id"));
        client
            .finish_chat_turn(
                &work.turn_id,
                slop_runtime::chat::ChatOutcome {
                    text: "answer".into(),
                    resolved_model: None,
                    usage: slop_runtime::providers::Usage::default(),
                    status: slop_runtime::chat::ChatStatus::Completed,
                    error_code: None,
                    error_message: None,
                },
            )
            .await
            .expect("finish");
    }
    let pending = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "context-overflow".into(),
                text: "last question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("queue pending");
    let work = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.turn_id, pending.turn_id.unwrap());
    assert!(work.context_overflow);
    assert!(work.history.len() <= MAX_CONTEXT_MESSAGES);
    assert!(
        !work
            .history
            .iter()
            .any(|m| m.visible_text() == "last question")
    );
    assert_eq!(client.turn(&work.turn_id).await.unwrap().status, "running");
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn startup_recovery_keeps_visible_checkpoint_out_of_context_and_marks_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("create-3", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-3".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("work");
    client
        .checkpoint_visible(&turn_id, "visible partial")
        .await
        .expect("checkpoint");
    store.shutdown().await.expect("shutdown");
    let store = open_test(&dir, None).await;
    let client = store.client();
    assert_eq!(
        client.turn(&turn_id).await.expect("turn").status,
        "interrupted"
    );
    let messages = client
        .messages(&created.session_id, None, 50)
        .await
        .expect("messages")
        .items;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].text, "visible partial");
    assert_eq!(messages[1].status, "interrupted");
    let events = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events");
    assert_eq!(
        events.items.last().expect("terminal event").kind,
        "turn_interrupted"
    );
    let session = client.session(&created.session_id).await.expect("session");
    let next = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "send-3b".into(),
                text: "new question".into(),
                expected_revision: Some(session.revision),

                ..Default::default()
            },
        )
        .await
        .expect("next message");
    let resumed = client
        .claim_next_turn()
        .await
        .expect("claim after recovery")
        .expect("queued turn");
    assert_eq!(resumed.turn_id, next.turn_id.expect("turn id"));
    assert_eq!(resumed.messages.len(), 2);
    assert_eq!(resumed.messages.last().unwrap().text, "new question");
    assert!(
        resumed
            .history
            .iter()
            .all(|m| m.role == slop_runtime::providers::inference::MessageRole::User)
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn completed_empty_output_replaces_a_nonempty_checkpoint() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("empty-create", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "empty-send".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("work");
    client
        .checkpoint_visible(&turn_id, "partial text")
        .await
        .expect("checkpoint");
    client
        .finish_chat_turn(
            &turn_id,
            slop_runtime::chat::ChatOutcome {
                text: String::new(),
                resolved_model: None,
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .expect("finish");
    let messages = client
        .messages(&created.session_id, None, 50)
        .await
        .expect("history")
        .items;
    assert_eq!(messages[1].text, "");
    assert_eq!(messages[1].status, "completed");
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn usage_counts_and_provenance_survive_restart() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("usage-create", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "usage-send".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    let work = client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("work");
    client
        .finish_chat_turn(
            &turn_id,
            slop_runtime::chat::ChatOutcome {
                text: "answer".into(),
                resolved_model: Some(work.requested_model),
                usage: slop_runtime::providers::Usage::from_reported(
                    Some(u64::MAX),
                    Some(7),
                    Some(u64::MAX),
                ),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .expect("finish");
    store.shutdown().await.expect("shutdown");

    let store = open_test(&dir, None).await;
    let turn = store.client().turn(&turn_id).await.expect("persisted turn");
    let usage = turn.usage.expect("usage");
    assert_eq!(usage.input_tokens, Some(u64::MAX));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.total_tokens, Some(u64::MAX));
    assert_eq!(usage.total_source.as_deref(), Some("reported"));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn session_pages_use_stable_creation_order_cursors() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let a = client
        .create_session(create_request("page-a", None))
        .await
        .expect("a");
    let b = client
        .create_session(create_request("page-b", None))
        .await
        .expect("b");
    let c = client
        .create_session(create_request("page-c", None))
        .await
        .expect("c");
    let first = client.sessions(None, 2).await.expect("first page");
    assert_eq!(
        first
            .items
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        [a.session_id.as_str(), b.session_id.as_str()]
    );
    let cursor = first.next_after.expect("next cursor");
    let second = client.sessions(Some(cursor), 2).await.expect("second page");
    assert_eq!(
        second
            .items
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        [c.session_id.as_str()]
    );
    assert_eq!(second.next_after, None);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn configured_default_is_persisted_without_changing_retry_payload() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let mut request = create_request("default-cap", None);
    request.max_tokens = None;
    let receipt = client
        .create_session_with_default_max_tokens(request.clone(), Some(12345))
        .await
        .expect("create");
    let retry = client
        .create_session_with_default_max_tokens(request, Some(54321))
        .await
        .expect("retry with changed daemon config");
    assert_eq!(receipt, retry);
    let session = client.session(&receipt.session_id).await.expect("session");
    assert_eq!(session.max_tokens, Some(12345));
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn queued_cancel_is_terminal_and_idempotent() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("cancel-queued-create", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "cancel-queued-send".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    let request = CancelTurnRequest {
        command_id: "cancel-queued".into(),
    };
    let receipt = client
        .cancel_turn(&turn_id, request.clone())
        .await
        .expect("cancel queued");
    let retry = client
        .cancel_turn(&turn_id, request)
        .await
        .expect("cancel retry");
    assert_eq!(receipt, retry);
    assert_eq!(
        client.turn(&turn_id).await.expect("turn").status,
        "cancelled"
    );
    assert!(client.claim_next_turn().await.expect("claim").is_none());
    let events = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events");
    assert_eq!(
        events.items.last().expect("cancel event").kind,
        "turn_cancelled"
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn accepted_running_cancel_overrides_later_completion_and_retries_original_receipt() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("cancel-running-create", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "cancel-running-send".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    let work = client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("work");
    let request = CancelTurnRequest {
        command_id: "cancel-running".into(),
    };
    let receipt = client
        .cancel_turn(&turn_id, request.clone())
        .await
        .expect("request cancellation");
    client
        .finish_chat_turn(
            &turn_id,
            slop_runtime::chat::ChatOutcome {
                text: "late completion".into(),
                resolved_model: Some(work.requested_model),
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .expect("cancellation wins");
    assert_eq!(
        client.turn(&turn_id).await.expect("turn").status,
        "cancelled"
    );
    assert_eq!(
        client
            .cancel_turn(&turn_id, request)
            .await
            .expect("retry after terminal state"),
        receipt
    );
    let events = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events");
    assert_eq!(
        events.items.last().expect("terminal event").kind,
        "turn_cancelled"
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn cancel_after_completion_conflicts_without_appending_event() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("cancel-done-create", None))
        .await
        .expect("create");
    let sent = client
        .send_message(
            &created.session_id,
            SendMessageRequest {
                command_id: "cancel-done-send".into(),
                text: "question".into(),
                expected_revision: None,

                ..Default::default()
            },
        )
        .await
        .expect("send");
    let turn_id = sent.turn_id.expect("turn id");
    let work = client
        .claim_next_turn()
        .await
        .expect("claim")
        .expect("work");
    client
        .finish_chat_turn(
            &turn_id,
            slop_runtime::chat::ChatOutcome {
                text: "complete".into(),
                resolved_model: Some(work.requested_model),
                usage: slop_runtime::providers::Usage::default(),
                status: slop_runtime::chat::ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .expect("finish");
    let before = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events before");
    assert!(matches!(
        client
            .cancel_turn(
                &turn_id,
                CancelTurnRequest {
                    command_id: "cancel-too-late".into(),
                }
            )
            .await,
        Err(StoreError::Conflict)
    ));
    let after = client
        .events(&created.session_id, 0, 50)
        .await
        .expect("events after");
    assert_eq!(before, after);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn command_id_reuse_across_operations_conflicts_without_mutation() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("cross-operation", None))
        .await
        .expect("create");
    assert!(matches!(
        client
            .send_message(
                &created.session_id,
                SendMessageRequest {
                    command_id: "cross-operation".into(),
                    text: "must not be inserted".into(),
                    expected_revision: None,

                    ..Default::default()
                }
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(
        client
            .messages(&created.session_id, None, 50)
            .await
            .expect("empty history")
            .items
            .is_empty()
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn message_pages_bound_escaped_json_and_resume_without_loss() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("escaped-page-create", None))
        .await
        .expect("create");
    store.shutdown().await.expect("close before fixture");

    let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
    for index in 1..=2 {
        let turn_id = format!("fixture-turn-{index}");
        let user_id = format!("fixture-user-{index}");
        let assistant_id = format!("fixture-assistant-{index}");
        conn.execute(
                "INSERT INTO turns(id,session_id,ordinal,user_message_id,status,requested_model) VALUES(?1,?2,?3,?4,'completed','opencode-go/glm-5.3-flash')",
                rusqlite::params![turn_id,created.session_id,index,user_id],
            )
            .expect("fixture turn");
        let escaped_text = "\u{0001}".repeat(MAX_CONTEXT_BYTES);
        conn.execute(
                "INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status) VALUES(?1,?2,?3,?4,'assistant',?5,'completed')",
                rusqlite::params![assistant_id,created.session_id,turn_id,index*2+1,escaped_text],
            )
            .expect("fixture message");
    }
    drop(conn);

    let store = open_test(&dir, None).await;
    let client = store.client();
    let first = client
        .messages(&created.session_id, None, 200)
        .await
        .expect("bounded first page");
    assert_eq!(first.items.len(), 1);
    assert!(first.next_after.is_some());
    assert!(serde_json::to_vec(&first).expect("encode page").len() <= MAX_MESSAGE_PAGE_BYTES);
    let second = client
        .messages(&created.session_id, first.next_after, 200)
        .await
        .expect("second page");
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.next_after, None);
    assert!(serde_json::to_vec(&second).expect("encode page").len() <= MAX_MESSAGE_PAGE_BYTES);
    assert_eq!(first.items[0].text, second.items[0].text);
    assert_eq!(first.items[0].text.len(), MAX_CONTEXT_BYTES);
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn send_event_transaction_failure_rolls_back_turn_message_and_receipt() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let client = store.client();
    let created = client
        .create_session(create_request("rollback-create", None))
        .await
        .expect("create");
    store
        .shutdown()
        .await
        .expect("close before fault injection");

    let path = dir.path().join(DATABASE_FILE);
    let conn = Connection::open(&path).expect("db");
    conn.execute_batch(
            "CREATE TRIGGER fail_user_event BEFORE INSERT ON session_events WHEN NEW.kind='user_message_accepted' BEGIN SELECT RAISE(ABORT,'injected event failure'); END;",
        )
        .expect("install trigger");
    drop(conn);

    let store = open_test(&dir, None).await;
    let client = store.client();
    let request = SendMessageRequest {
        command_id: "rollback-send".into(),
        text: "atomic message".into(),
        expected_revision: None,

        ..Default::default()
    };
    assert!(matches!(
        client
            .send_message(&created.session_id, request.clone())
            .await,
        Err(StoreError::Database)
    ));
    store
        .shutdown()
        .await
        .expect("shutdown after injected failure");

    let conn = Connection::open(&path).expect("db");
    let turns: i64 = conn
        .query_row(
            "SELECT count(*) FROM turns WHERE session_id=?1",
            [&created.session_id],
            |row| row.get(0),
        )
        .expect("turn count");
    let messages: i64 = conn
        .query_row(
            "SELECT count(*) FROM messages WHERE session_id=?1",
            [&created.session_id],
            |row| row.get(0),
        )
        .expect("message count");
    let commands: i64 = conn
        .query_row(
            "SELECT count(*) FROM commands WHERE command_id='rollback-send'",
            [],
            |row| row.get(0),
        )
        .expect("receipt count");
    assert_eq!((turns, messages, commands), (0, 0, 0));
    conn.execute_batch("DROP TRIGGER fail_user_event;")
        .expect("remove trigger");
    drop(conn);

    let store = open_test(&dir, None).await;
    let receipt = store
        .client()
        .send_message(&created.session_id, request)
        .await
        .expect("retry after rollback");
    assert!(receipt.turn_id.is_some());
    assert_eq!(
        store
            .client()
            .messages(&created.session_id, None, 50)
            .await
            .expect("committed message")
            .items
            .len(),
        1
    );
    store.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn second_store_owner_is_rejected_and_shutdown_releases_lock() {
    let dir = TempDir::new().expect("temp dir");
    let store = open_test(&dir, None).await;
    let err = Store::open(
        dir.path().to_path_buf(),
        None,
        2,
        Duration::from_millis(100),
    )
    .await
    .err()
    .expect("only one owner");
    assert!(matches!(err, StoreError::AlreadyRunning));
    store.shutdown().await.expect("shutdown");
    let reopened = open_test(&dir, None).await;
    reopened.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn bounded_queue_reports_busy_and_shutdown_drains_accepted_calls() {
    let dir = TempDir::new().expect("temp dir");
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
            .expect("private temp dir");
    }
    let store = Store::open(
        dir.path().to_path_buf(),
        None,
        1,
        Duration::from_millis(200),
    )
    .await
    .expect("store opens");
    let client = store.client();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    client
        .shared
        .sender
        .try_send(Request::Hold {
            started: started_tx,
            release: release_rx,
        })
        .expect("worker hold accepted");
    started_rx.await.expect("worker reached hold");

    let (reply_tx, reply_rx) = oneshot::channel();
    client
        .shared
        .sender
        .try_send(Request::Node(reply_tx))
        .expect("one request fits bounded queue");
    assert!(matches!(client.node().await, Err(StoreError::Busy)));

    let shutdown = tokio::spawn(store.shutdown());
    tokio::task::yield_now().await;
    release_tx.send(()).expect("release worker");
    let result = reply_rx
        .await
        .expect("accepted request replied")
        .expect("node");
    assert!(!result.node_id.is_empty());
    shutdown
        .await
        .expect("shutdown task")
        .expect("clean shutdown");
    assert!(matches!(client.node().await, Err(StoreError::Unavailable)));
}

#[tokio::test]
async fn dropping_shutdown_future_keeps_lock_until_worker_closes() {
    let dir = TempDir::new().expect("temp dir");
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
            .expect("private temp dir");
    }
    let store = Store::open(
        dir.path().to_path_buf(),
        None,
        1,
        Duration::from_millis(200),
    )
    .await
    .expect("store opens");
    let client = store.client();
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    assert!(
        client
            .shared
            .sender
            .try_send(Request::Hold {
                started: started_tx,
                release: release_rx,
            })
            .is_ok()
    );
    started_rx.await.expect("worker reached hold");

    let (reply_tx, reply_rx) = oneshot::channel();
    assert!(
        client
            .shared
            .sender
            .try_send(Request::Node(reply_tx))
            .is_ok()
    );
    let shutdown = tokio::spawn(store.shutdown());
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if matches!(client.node().await, Err(StoreError::Unavailable)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("shutdown started");
    shutdown.abort();
    let _ = shutdown.await;

    let err = Store::open(
        dir.path().to_path_buf(),
        None,
        2,
        Duration::from_millis(100),
    )
    .await
    .err()
    .expect("worker still owns lock");
    assert!(matches!(err, StoreError::AlreadyRunning));

    release_tx.send(()).expect("release worker");
    reply_rx
        .await
        .expect("accepted request completed")
        .expect("identity query");
    let reopened = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match Store::open(
                dir.path().to_path_buf(),
                None,
                2,
                Duration::from_millis(100),
            )
            .await
            {
                Ok(store) => break store,
                Err(StoreError::AlreadyRunning) => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("reopen failed: {error}"),
            }
        }
    })
    .await
    .expect("worker eventually released lock");
    reopened.shutdown().await.expect("shutdown");
}

#[test]
fn newer_schema_is_rejected_before_journal_mode_changes() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(DATABASE_FILE);
    let conn = Connection::open(&path).expect("db");
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .expect("version");
    drop(conn);
    let before = fs::read(&path).expect("database bytes");
    let result = open_and_initialize(dir.path(), None, Duration::from_millis(100));
    assert!(matches!(result, Err(StoreError::NewerSchema { .. })));
    let after = fs::read(&path).expect("database bytes");
    assert_eq!(before, after);
    assert!(!dir.path().join(format!("{DATABASE_FILE}-wal")).exists());
    assert!(!dir.path().join(format!("{DATABASE_FILE}-shm")).exists());
}

#[test]
fn negative_schema_version_is_rejected_without_mutation() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(DATABASE_FILE);
    let conn = Connection::open(&path).expect("db");
    conn.pragma_update(None, "user_version", -1)
        .expect("version");
    drop(conn);
    let before = fs::read(&path).expect("database bytes");
    assert!(matches!(
        open_and_initialize(dir.path(), None, Duration::from_millis(100)),
        Err(StoreError::Database)
    ));
    assert_eq!(fs::read(&path).expect("database bytes"), before);
    assert!(!dir.path().join(format!("{DATABASE_FILE}-wal")).exists());
}

#[test]
fn invalid_existing_identity_is_not_modified_by_name_override() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(DATABASE_FILE);
    let conn = Connection::open(&path).expect("db");
    migrate(&conn, 0, Some("original".to_owned())).expect("migration");
    conn.execute("UPDATE node_identity SET node_id = 'invalid'", [])
        .expect("invalid fixture");
    drop(conn);

    assert!(matches!(
        open_and_initialize(
            dir.path(),
            Some("replacement".to_owned()),
            Duration::from_millis(100)
        ),
        Err(StoreError::Database)
    ));
    let conn = Connection::open(path).expect("db");
    let name: String = conn
        .query_row(
            "SELECT name FROM node_identity WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .expect("identity name");
    assert_eq!(name, "original");
}

#[test]
fn migration_failure_rolls_back_identity_and_schema() {
    let dir = TempDir::new().expect("temp dir");
    let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
    let result = migrate_with_hook(&conn, 0, Some("partial".to_owned()), |tx| {
        tx.execute(
            "INSERT INTO node_identity VALUES (1, 'injected', 'partial', 'test')",
            [],
        )
        .map_err(|_| StoreError::Database)?;
        Err(StoreError::Database)
    });
    assert!(result.is_err());
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='node_identity'",
            [],
            |row| row.get(0),
        )
        .expect("table count");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("version");
    assert_eq!(tables, 0);
    assert_eq!(version, 0);
}

#[test]
fn corrupt_existing_database_is_not_recreated() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(DATABASE_FILE);
    fs::write(&path, b"not sqlite").expect("corrupt bytes");
    let before = fs::read(&path).expect("before");
    assert!(open_and_initialize(dir.path(), None, Duration::from_millis(100)).is_err());
    assert_eq!(fs::read(&path).expect("after"), before);
}

#[cfg(unix)]
#[test]
fn database_and_lock_symlinks_are_rejected() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new().expect("temp dir");
    let target = dir.path().join("other-file");
    fs::write(&target, b"keep").expect("target");
    symlink(&target, dir.path().join(DATABASE_FILE)).expect("database symlink");
    assert!(matches!(
        open_and_initialize(dir.path(), None, Duration::from_millis(100)),
        Err(StoreError::Configuration)
    ));
    fs::remove_file(dir.path().join(DATABASE_FILE)).expect("remove db link");
    symlink(&target, dir.path().join(LOCK_FILE)).expect("lock symlink");
    assert!(matches!(
        open_lock(dir.path()),
        Err(StoreError::Configuration)
    ));
    assert_eq!(fs::read(target).expect("target remains"), b"keep");
}

#[test]
fn version_zero_database_with_unrecognized_schema_is_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
    conn.execute("CREATE TABLE unrelated (value TEXT)", [])
        .expect("unrecognized schema");
    drop(conn);
    assert!(matches!(
        open_and_initialize(dir.path(), None, Duration::from_millis(100)),
        Err(StoreError::Database)
    ));
}

#[test]
fn migration_wrapper_uses_one_transaction() {
    let dir = TempDir::new().expect("temp dir");
    let conn = Connection::open(dir.path().join(DATABASE_FILE)).expect("db");
    migrate(&conn, 0, None).expect("migration");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("version");
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn schema_one_migration_preserves_node_identity_and_adds_chat_tables() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join(DATABASE_FILE);
    let conn = Connection::open(&path).expect("db");
    conn.execute_batch(
            "CREATE TABLE node_identity (singleton INTEGER PRIMARY KEY CHECK(singleton=1),node_id TEXT NOT NULL UNIQUE,name TEXT NOT NULL,os TEXT NOT NULL);
             INSERT INTO node_identity VALUES(1,'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa','legacy','linux');
             PRAGMA user_version=1;",
        )
        .expect("schema one fixture");
    drop(conn);

    let conn = open_and_initialize(dir.path(), None, Duration::from_millis(100))
        .expect("migrate schema one");
    let identity = query_node(&conn).expect("identity");
    assert_eq!(identity.node_id, "a".repeat(64));
    assert_eq!(identity.name, "legacy");
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .expect("schema version");
    assert_eq!(version, SCHEMA_VERSION);
    let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions','turns','messages','session_events','commands')",
                [],
                |row| row.get(0),
            )
            .expect("chat tables");
    assert_eq!(tables, 5);
}

#[test]
fn initialized_database_has_required_pragmas_and_supported_sqlite() {
    let dir = TempDir::new().expect("temp dir");
    let conn =
        open_and_initialize(dir.path(), None, Duration::from_millis(137)).expect("initialized db");
    verify_pragmas(&conn, Duration::from_millis(137)).expect("pragmas");
    assert!(rusqlite::version_number() >= MINIMUM_SQLITE_VERSION);
}

fn migrate_with_hook<F>(
    connection: &Connection,
    existing_version: i64,
    node_name: Option<String>,
    hook: F,
) -> StoreResult<()>
where
    F: FnOnce(&Transaction<'_>) -> StoreResult<()>,
{
    super::migrate_with_hook(connection, existing_version, node_name, hook)
}
