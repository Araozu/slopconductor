use super::*;
use crate::storage::tests::{create_request, open_test};
use slop_runtime::{
    chat::{ChatOutcome, ChatStatus},
    providers::{Usage, inference::*},
};
use tempfile::TempDir;

async fn complete(
    client: &StoreClient,
    session: &str,
    command: &str,
    prompt: &str,
    reply: &str,
) -> CommandReceipt {
    let receipt = client
        .send_message(
            session,
            SendMessageRequest {
                command_id: command.into(),
                text: prompt.into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let work = client.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.turn_id, *receipt.turn_id.as_ref().unwrap());
    client
        .finish_chat_turn(
            &work.turn_id,
            ChatOutcome {
                text: reply.into(),
                resolved_model: Some(work.requested_model),
                usage: Usage::default(),
                status: ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    receipt
}

#[tokio::test]
async fn forks_and_edits_copy_only_the_prefix_and_survive_source_deletion_and_restart() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("source", Some("Original")))
        .await
        .unwrap()
        .session_id;
    complete(&c, &source, "one", "first prompt", "first reply").await;
    complete(&c, &source, "two", "later prompt", "later reply").await;
    let original = c.messages(&source, None, 200).await.unwrap().items;
    let revision = c.session(&source).await.unwrap().revision;
    let fork_request = ForkSessionRequest {
        command_id: "fork".into(),
        message_id: original[1].id.clone(),
        expected_revision: Some(revision),
        title: Some("Branch".into()),
    };
    let fork = c.fork_session(&source, fork_request.clone()).await.unwrap();
    assert_eq!(
        c.fork_session(&source, fork_request.clone()).await.unwrap(),
        fork
    );
    let copied = c.messages(&fork.session_id, None, 200).await.unwrap().items;
    assert_eq!(
        copied.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
        ["first prompt", "first reply"]
    );
    assert!(copied.iter().all(|m| {
        original
            .iter()
            .all(|old| old.id != m.id && old.turn_id != m.turn_id)
    }));
    assert_eq!(fork.message_id.as_ref(), Some(&copied[1].id));
    let snapshot = c.session(&fork.session_id).await.unwrap();
    assert_eq!(snapshot.title.as_deref(), Some("Branch"));
    assert_eq!(snapshot.fork.as_ref().unwrap().message_id, original[1].id);
    assert!(c.claim_next_turn().await.unwrap().is_none());
    let edit_request = EditMessageRequest {
        command_id: "edit".into(),
        text: "replacement prompt".into(),
        expected_revision: Some(revision),
    };
    let edit = c
        .edit_message(&source, &original[2].id, edit_request.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        c.edit_message(&source, &original[2].id, edit_request.clone(), None)
            .await
            .unwrap(),
        edit
    );
    let work = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(
        work.history
            .iter()
            .map(InferenceMessage::visible_text)
            .collect::<Vec<_>>(),
        ["first prompt", "first reply", "replacement prompt"]
    );
    c.finish_chat_turn(
        &work.turn_id,
        ChatOutcome {
            text: "new reply".into(),
            resolved_model: None,
            usage: Usage::default(),
            status: ChatStatus::Completed,
            error_code: None,
            error_message: None,
        },
    )
    .await
    .unwrap();
    let assistant_edit = c
        .edit_message(
            &source,
            &original[1].id,
            EditMessageRequest {
                command_id: "edit-assistant".into(),
                text: "corrected answer".into(),
                expected_revision: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(assistant_edit.turn_id.is_none());
    assert_eq!(
        c.messages(&assistant_edit.session_id, None, 200)
            .await
            .unwrap()
            .items[1]
            .text,
        "corrected answer"
    );
    assert_eq!(
        c.messages(&source, None, 200).await.unwrap().items,
        original
    );
    let delete_request = DeleteMessageRequest {
        command_id: "delete-source".into(),
        expected_revision: Some(revision),
    };
    c.delete_message(&source, &original[0].id, delete_request.clone())
        .await
        .unwrap();
    assert!(
        c.messages(&source, None, 200)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        c.message(&original[0].id).await,
        Err(StoreError::NotFound)
    ));
    assert_eq!(
        c.fork_session(&source, fork_request.clone()).await.unwrap(),
        fork
    );
    assert_eq!(
        c.edit_message(&source, &original[2].id, edit_request.clone(), None)
            .await
            .unwrap(),
        edit
    );
    let mut conflict = fork_request.clone();
    conflict.message_id = original[0].id.clone();
    assert!(matches!(
        c.fork_session(&source, conflict).await,
        Err(StoreError::Conflict)
    ));
    store.shutdown().await.unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    assert_eq!(
        c.messages(&fork.session_id, None, 200).await.unwrap().items,
        copied
    );
    assert!(
        c.messages(&source, None, 200)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert_eq!(c.fork_session(&source, fork_request).await.unwrap(), fork);
    assert_eq!(
        c.edit_message(&source, &original[2].id, edit_request, None)
            .await
            .unwrap(),
        edit
    );
    assert!(c.claim_next_turn().await.unwrap().is_none());
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn deletion_invalidates_summaries_preserves_cursors_and_rejects_busy_or_stale_commands() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("source", None))
        .await
        .unwrap()
        .session_id;
    let first = complete(&c, &source, "first", "keep prompt", "keep reply").await;
    let last = complete(&c, &source, "last", "remove prompt", "remove reply").await;
    let messages = c.messages(&source, None, 200).await.unwrap().items;
    let revision = c.session(&source).await.unwrap().revision;
    // Install a real request-linked compacted checkpoint with facts from the suffix.
    let session = source.clone();
    let turn = last.turn_id.clone().unwrap();
    c.submit(move |db| {
        db.execute("INSERT INTO model_requests(id,turn_id,message_id,ordinal,requested_model,requested_settings,settings,status) VALUES('summary-request',?1,?2,1,'opencode-go/glm-5.3-flash','{}','{}','completed')", rusqlite::params![turn,last.message_id.unwrap()]).unwrap();
        db.execute("INSERT INTO session_context(session_id,through_ordinal,messages,request_id) VALUES(?1,2049,?2,'summary-request')", rusqlite::params![session, canonical(&vec![InferenceMessage::text(MessageRole::User,"summary".into(),"remove prompt and remove reply".into())])?]).unwrap();
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        c.delete_message(
            &source,
            &messages[2].id,
            DeleteMessageRequest {
                command_id: "stale".into(),
                expected_revision: Some(revision - 1)
            }
        )
        .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        c.fork_session(
            &source,
            ForkSessionRequest {
                command_id: "wrong-session".into(),
                message_id: "missing".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::NotFound)
    ));
    let request = DeleteMessageRequest {
        command_id: "delete".into(),
        expected_revision: Some(revision),
    };
    let receipt = c
        .delete_message(&source, &messages[2].id, request.clone())
        .await
        .unwrap();
    assert_eq!(
        c.delete_message(&source, &messages[2].id, request)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        c.messages(&source, None, 1).await.unwrap().next_after,
        Some(1024)
    );
    assert_eq!(
        c.messages(&source, Some(1024), 200).await.unwrap().items,
        vec![messages[1].clone()]
    );
    assert_eq!(c.session(&source).await.unwrap().spec.prompt, "keep prompt");
    let events = c.events(&source, revision, 200).await.unwrap().items;
    assert_eq!(events.last().unwrap().kind, "messages_deleted");
    assert!(receipt.event_sequence > revision);
    let next = c
        .send_message(
            &source,
            SendMessageRequest {
                command_id: "follow".into(),
                text: "fresh prompt".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        c.delete_message(
            &source,
            first.message_id.as_ref().unwrap(),
            DeleteMessageRequest {
                command_id: "busy".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::HistoryBusy)
    ));
    let work = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.turn_id, next.turn_id.unwrap());
    assert_eq!(
        work.history
            .iter()
            .map(InferenceMessage::visible_text)
            .collect::<Vec<_>>(),
        ["keep prompt", "keep reply", "fresh prompt"]
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn cuts_inside_tool_exchanges_never_replay_unpaired_tools_or_private_continuation() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("source", None))
        .await
        .unwrap()
        .session_id;
    let accepted = complete(&c, &source, "tool-turn", "inspect files", "final reply").await;
    let session = source.clone();
    let turn = accepted.turn_id.unwrap();
    // A completed tool exchange with two proposals and two paired results.
    let blocks = vec!["a", "b"]
        .into_iter()
        .map(|id| ContentBlock {
            id: id.into(),
            content: BlockContent::ToolCall {
                call_id: id.into(),
                provider_call_id: format!("private-{id}"),
                name: "read".into(),
                arguments: serde_json::json!({"path":"file"}),
            },
        })
        .collect::<Vec<_>>();
    c.submit(move |db| {
        db.execute("UPDATE messages SET ordinal=1028 WHERE turn_id=?1 AND role='assistant'",[&turn]).unwrap();
        db.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,blocks,continuation,request_id) VALUES('calls',?1,?2,1025,'assistant','','completed',?3,?4,'request')",rusqlite::params![session,turn,canonical(&blocks)?,canonical(&Continuation{scope:"scope".into(),provider:"codex".into(),model:"gpt-6.1-sol".into(),wire:"responses".into(),required:true,items:vec![serde_json::json!({"secret":"opaque"})]})?]).unwrap();
        db.execute("INSERT INTO model_requests(id,turn_id,message_id,ordinal,requested_model,requested_settings,settings,status) VALUES('request',?1,'calls',1,'opencode-go/glm-5.3-flash','{}','{}','completed')",[&turn]).unwrap();
        for (offset,id) in ["a","b"].into_iter().enumerate() {
            let blocks=vec![ContentBlock{id:format!("result-{id}"),content:BlockContent::ToolResult{call_id:id.into(),provider_call_id:format!("private-{id}"),is_error:false,output:format!("result {id}"),artifact_ids:vec![],effects_unknown:false}}];
            db.execute("INSERT INTO messages(id,session_id,turn_id,ordinal,role,text,status,blocks,request_id) VALUES(?1,?2,?3,?4,'tool',?5,'completed',?6,'request')",rusqlite::params![format!("result-{id}"),session,turn,1026+offset as i64,format!("result {id}"),canonical(&blocks)?]).unwrap();
        }
        Ok(())
    }).await.unwrap();
    for (index, message) in ["calls", "result-a", "result-b"].into_iter().enumerate() {
        let fork = c
            .fork_session(
                &source,
                ForkSessionRequest {
                    command_id: format!("fork-{index}"),
                    message_id: message.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            c.messages(&fork.session_id, None, 200)
                .await
                .unwrap()
                .items
                .len(),
            index + 2
        );
        let requirements = c.context_requirements(&fork.session_id).await.unwrap();
        assert_eq!(!requirements.1.is_empty(), index == 2);
        c.send_message(
            &fork.session_id,
            SendMessageRequest {
                command_id: format!("follow-{index}"),
                text: "continue".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let work = c.claim_next_turn().await.unwrap().unwrap();
        assert!(slop_runtime::agent::context::safe_boundary(
            &work.history,
            work.history.len()
        ));
        assert_eq!(work.history.len(), if index == 2 { 5 } else { 2 });
        c.finish_chat_turn(
            &work.turn_id,
            ChatOutcome {
                text: "done".into(),
                resolved_model: None,
                usage: Usage::default(),
                status: ChatStatus::Completed,
                error_code: None,
                error_message: None,
            },
        )
        .await
        .unwrap();
    }
    let later = complete(
        &c,
        &source,
        "after-tools",
        "after paired tools",
        "later reply",
    )
    .await;
    let session_count = c.sessions(None, 200).await.unwrap().items.len();
    assert!(matches!(
        c.edit_message(
            &source,
            later.message_id.as_ref().unwrap(),
            EditMessageRequest {
                command_id: "incompatible-edit".into(),
                text: "replacement".into(),
                ..Default::default()
            },
            None
        )
        .await,
        Err(StoreError::Preflight("context_incompatible", _))
    ));
    assert_eq!(
        c.sessions(None, 200).await.unwrap().items.len(),
        session_count
    );
    c.delete_message(
        &source,
        "result-b",
        DeleteMessageRequest {
            command_id: "delete-result".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(c.context_requirements(&source).await.unwrap().1.is_empty());
    c.send_message(
        &source,
        SendMessageRequest {
            command_id: "follow-trim".into(),
            text: "continue safely".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let work = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(work.history.len(), 2);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema_eleven_migrates_without_changing_existing_history() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("source", None))
        .await
        .unwrap()
        .session_id;
    complete(&c, &source, "message", "old prompt", "old reply").await;
    let original = c.messages(&source, None, 200).await.unwrap().items;
    c.submit(|db|{
        db.execute_batch("DROP INDEX messages_request; ALTER TABLE sessions DROP COLUMN fork; ALTER TABLE turns DROP COLUMN history_only; ALTER TABLE messages DROP COLUMN deleted; ALTER TABLE messages DROP COLUMN context_excluded; PRAGMA user_version=11;").unwrap(); Ok(())
    }).await.unwrap();
    store.shutdown().await.unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    assert_eq!(
        c.messages(&source, None, 200).await.unwrap().items,
        original
    );
    assert!(c.session(&source).await.unwrap().fork.is_none());
    assert!(
        c.fork_session(
            &source,
            ForkSessionRequest {
                command_id: "fork-migrated".into(),
                message_id: original[1].id.clone(),
                ..Default::default()
            }
        )
        .await
        .is_ok()
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn forking_running_work_freezes_only_the_committed_snapshot() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let mut create = create_request("active-source", None);
    create.prompt = Some("initial prompt".into());
    let accepted = c.create_session(create).await.unwrap();
    let work = c.claim_next_turn().await.unwrap().unwrap();
    c.checkpoint_visible(&work.turn_id, "committed partial")
        .await
        .unwrap();
    let source_messages = c
        .messages(&accepted.session_id, None, 200)
        .await
        .unwrap()
        .items;
    let fork = c
        .fork_session(
            &accepted.session_id,
            ForkSessionRequest {
                command_id: "active-fork".into(),
                message_id: source_messages[1].id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(c.claim_next_turn().await.unwrap().is_none());
    let snapshot = c.messages(&fork.session_id, None, 200).await.unwrap().items;
    assert_eq!(snapshot[1].status, "interrupted");
    c.finish_chat_turn(
        &work.turn_id,
        ChatOutcome {
            text: "completed response".into(),
            resolved_model: None,
            usage: Usage::default(),
            status: ChatStatus::Completed,
            error_code: None,
            error_message: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        c.messages(&fork.session_id, None, 200).await.unwrap().items,
        snapshot
    );
    c.send_message(
        &fork.session_id,
        SendMessageRequest {
            command_id: "continue-fork".into(),
            text: "new instruction".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let continuation = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(
        continuation
            .history
            .iter()
            .map(InferenceMessage::visible_text)
            .collect::<Vec<_>>(),
        ["initial prompt", "new instruction"]
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn event_failure_rolls_back_history_mutations_and_deletion_does_not_refund_budgets() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("source", None))
        .await
        .unwrap()
        .session_id;
    complete(&c, &source, "prompt", "original", "reply").await;
    let original = c.messages(&source, None, 200).await.unwrap().items;
    c.submit(|db| {
        db.execute_batch(
            "UPDATE operation_budgets SET model_requests=3,tool_calls=2;
            CREATE TRIGGER fail_history_event BEFORE INSERT ON session_events
            WHEN NEW.kind IN('session_forked','message_edited','messages_deleted')
            BEGIN SELECT RAISE(ABORT,'fixture event failure'); END;",
        )
        .unwrap();
        Ok(())
    })
    .await
    .unwrap();
    assert!(matches!(
        c.fork_session(
            &source,
            ForkSessionRequest {
                command_id: "failed-fork".into(),
                message_id: original[1].id.clone(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Database)
    ));
    assert!(matches!(
        c.edit_message(
            &source,
            &original[0].id,
            EditMessageRequest {
                command_id: "failed-edit".into(),
                text: "replacement".into(),
                ..Default::default()
            },
            None
        )
        .await,
        Err(StoreError::Database)
    ));
    assert!(matches!(
        c.delete_message(
            &source,
            &original[0].id,
            DeleteMessageRequest {
                command_id: "failed-delete".into(),
                ..Default::default()
            }
        )
        .await,
        Err(StoreError::Database)
    ));
    assert_eq!(c.sessions(None, 200).await.unwrap().items.len(), 1);
    assert_eq!(
        c.messages(&source, None, 200).await.unwrap().items,
        original
    );
    for command in ["failed-fork", "failed-edit", "failed-delete"] {
        assert!(!c.command_seen(command).await.unwrap());
    }
    c.submit(|db| {
        db.execute_batch("DROP TRIGGER fail_history_event;")
            .unwrap();
        Ok(())
    })
    .await
    .unwrap();
    let before = c.session(&source).await.unwrap().budget_usage;
    c.delete_message(
        &source,
        &original[0].id,
        DeleteMessageRequest {
            command_id: "delete".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(c.session(&source).await.unwrap().budget_usage, before);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn legacy_managed_sessions_fork_into_a_separate_workspace() {
    let dir = TempDir::new().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let source = c
        .create_session(create_request("legacy-source", None))
        .await
        .unwrap()
        .session_id;
    complete(&c, &source, "legacy-prompt", "prompt", "reply").await;
    let messages = c.messages(&source, None, 200).await.unwrap().items;
    let root = dir.path().join("legacy-workspace");
    fs::create_dir(&root).unwrap();
    let policy = slop_protocol::execution::WorkspacePolicy {
        root: root.to_str().unwrap().into(),
        allowed_tools: vec!["read".into()],
        shell_timeout_ms: 30_000,
        max_output_bytes: 1024 * 1024,
        max_tool_calls: 32,
        max_model_requests: 16,
    };
    let session = source.clone();
    c.submit(move |db| {
        let node=query_node(db)?;
        db.execute("INSERT INTO projects(id,owner_node_id,path,git_common_dir) VALUES('legacy-project',?1,?2,?3)", rusqlite::params![node.node_id,policy.root,root.join(".git").to_str().unwrap()]).unwrap();
        db.execute("INSERT INTO managed_workspaces(id,project_id,session_id,owner_node_id,path,base_commit,status) VALUES('legacy-workspace','legacy-project',?1,?2,?3,?4,'ready')",rusqlite::params![session,node.node_id,policy.root,"a".repeat(40)]).unwrap();
        db.execute("UPDATE sessions SET managed_workspace_id='legacy-workspace',execution=?1,workspace_root=?2 WHERE id=?3",rusqlite::params![canonical(&policy)?,policy.root,session]).unwrap();
        Ok(())
    }).await.unwrap();
    assert!(c.session(&source).await.unwrap().spec.project.is_none());
    let fork = c
        .fork_session(
            &source,
            ForkSessionRequest {
                command_id: "legacy-fork".into(),
                message_id: messages[1].id.clone(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let branch = c.session(&fork.session_id).await.unwrap();
    let workspace = c
        .workspace(branch.managed_workspace_id.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(workspace.status, "reserved");
    assert_eq!(workspace.base_commit, "a".repeat(40));
    assert_ne!(workspace.id, "legacy-workspace");
    assert_ne!(
        workspace.path,
        c.session(&source).await.unwrap().execution.unwrap().root
    );
    assert_eq!(branch.spec.project.unwrap().allowed_tools, vec!["read"]);
    store.shutdown().await.unwrap();
}
