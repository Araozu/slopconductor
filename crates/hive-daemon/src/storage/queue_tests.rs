use super::tests::{create_request, open_test};
use super::*;
use hive_protocol::chat::{InterruptSessionRequest, UpdateQueuedMessageRequest};

#[tokio::test]
async fn queue_edits_preserve_fifo_and_retries_never_cancel_dispatched_work() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("edit-queue", None))
        .await
        .unwrap()
        .session_id;
    send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
    let first = c.claim_next_turn().await.unwrap().unwrap();
    let request = start_request(&c, &first, "edit-queue").await;
    let queued = send(&c, &session, "original", DeliveryMode::AfterTurn).await;
    let message = queued.message_id.unwrap();
    let removed = send(&c, &session, "remove", DeliveryMode::AfterFinish)
        .await
        .message_id
        .unwrap();
    let edit = UpdateQueuedMessageRequest {
        command_id: "edit-once".into(),
        text: Some("edited".into()),
        delivery: Some(DeliveryMode::AfterFinish),
    };
    let receipt = c
        .update_queued_message(&session, &message, edit.clone())
        .await
        .unwrap();
    let queue = c.queued_messages(&session).await.unwrap();
    assert_eq!(
        queue.iter().map(|q| q.id.as_str()).collect::<Vec<_>>(),
        [message.as_str(), removed.as_str()]
    );
    assert_eq!(queue[0].text, "edited");
    assert_eq!(queue[0].delivery, DeliveryMode::AfterFinish);
    let other = c
        .create_session(create_request("other-queue", None))
        .await
        .unwrap()
        .session_id;
    assert!(matches!(
        c.update_queued_message(
            &other,
            &message,
            UpdateQueuedMessageRequest {
                command_id: "wrong-session".into(),
                ..edit.clone()
            }
        )
        .await,
        Err(StoreError::NotFound)
    ));
    for (id, text, delivery) in [
        ("empty", Some(" ".into()), None),
        ("empty-patch", None, None),
        ("immediate", None, Some(DeliveryMode::Immediate)),
        ("boundary", None, Some(DeliveryMode::NextBoundary)),
    ] {
        assert!(matches!(
            c.update_queued_message(
                &session,
                &message,
                UpdateQueuedMessageRequest {
                    command_id: id.into(),
                    text,
                    delivery
                }
            )
            .await,
            Err(StoreError::Invalid)
        ));
    }
    assert!(matches!(
        c.update_queued_message(
            &session,
            &message,
            UpdateQueuedMessageRequest {
                text: Some("conflicting retry".into()),
                ..edit.clone()
            }
        )
        .await,
        Err(StoreError::Conflict)
    ));
    let cancel = CancelTurnRequest {
        command_id: "remove-once".into(),
    };
    let cancelled = c
        .cancel_queued_message(&session, &removed, cancel.clone())
        .await
        .unwrap();
    assert_eq!(
        c.cancel_queued_message(&session, &removed, cancel)
            .await
            .unwrap(),
        cancelled
    );
    assert!(
        !c.turn_control(&first.turn_id)
            .await
            .unwrap()
            .cancellation_requested
    );
    complete_request(&c, &first, request).await;
    c.finish_chat_turn(&first.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    let next = c.claim_next_turn().await.unwrap().unwrap();
    assert_ne!(
        next.turn_id, first.turn_id,
        "delivery mode changed to after-finish"
    );
    assert_eq!(next.history.last().unwrap().visible_text(), "edited");
    assert!(c.queued_messages(&session).await.unwrap().is_empty());
    assert_eq!(
        c.update_queued_message(&session, &message, edit.clone())
            .await
            .unwrap(),
        receipt
    );
    assert!(matches!(
        c.update_queued_message(
            &session,
            &message,
            UpdateQueuedMessageRequest {
                command_id: "too-late-edit".into(),
                ..edit
            }
        )
        .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        c.cancel_queued_message(
            &session,
            &message,
            CancelTurnRequest {
                command_id: "too-late-cancel".into()
            }
        )
        .await,
        Err(StoreError::Conflict)
    ));
    assert!(
        !c.turn_control(&next.turn_id)
            .await
            .unwrap()
            .cancellation_requested
    );
    store.shutdown().await.unwrap();
}
use hive_runtime::{
    agent::{RequestCompletion, RequestIntent},
    chat::{ChatOutcome, ChatRepository, ChatStatus, TurnWork},
    providers::inference::{
        ContentBlock, FinishReason, InferenceMessage, InferenceResponse, MessageRole,
    },
};

fn outcome(status: ChatStatus) -> ChatOutcome {
    ChatOutcome {
        text: "response".into(),
        resolved_model: None,
        usage: Default::default(),
        status,
        error_code: None,
        error_message: None,
    }
}

async fn send(
    c: &StoreClient,
    session: &str,
    text: &str,
    delivery: DeliveryMode,
) -> CommandReceipt {
    c.send_message(
        session,
        SendMessageRequest {
            command_id: format!("queue-{text}"),
            text: text.into(),
            delivery: Some(delivery),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

async fn start_request(c: &StoreClient, work: &TurnWork, suffix: &str) -> String {
    let id = format!("request-{suffix}");
    assert!(
        c.begin_request(
            &work.turn_id,
            RequestIntent {
                id: id.clone(),
                message_id: format!("assistant-{suffix}"),
                ordinal: work.next_request_ordinal,
                requested_model: work.requested_model.clone(),
                requested_settings: work.requested_settings.clone(),
                settings: work.settings.clone(),
                context: Default::default()
            }
        )
        .await
        .unwrap()
    );
    id
}

async fn complete_request(c: &StoreClient, work: &TurnWork, id: String) {
    c.complete_request(
        &work.turn_id,
        RequestCompletion {
            request_id: id,
            response: InferenceResponse {
                message: InferenceMessage {
                    role: MessageRole::Assistant,
                    blocks: vec![ContentBlock {
                        id: "text".into(),
                        content: hive_runtime::providers::inference::BlockContent::Text {
                            text: "response".into(),
                        },
                    }],
                    continuation: None,
                },
                finish_reason: FinishReason::Stop,
                resolved_model: None,
                usage: Default::default(),
            },
            tools: vec![],
            compaction: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn boundary_batches_stop_at_finish_and_finish_starts_alone() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("queue-create", None))
        .await
        .unwrap()
        .session_id;
    send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
    let first = c.claim_next_turn().await.unwrap().unwrap();
    let request = start_request(&c, &first, "initial").await;
    let a = send(&c, &session, "a", DeliveryMode::AfterTurn).await;
    send(&c, &session, "b", DeliveryMode::AfterTurn).await;
    let finish = send(&c, &session, "finish", DeliveryMode::AfterFinish).await;
    send(&c, &session, "later", DeliveryMode::AfterTurn).await;
    assert_eq!(c.queued_messages(&session).await.unwrap().len(), 4);
    assert!(
        !c.turn_control(&first.turn_id)
            .await
            .unwrap()
            .steering_pending,
        "generation must finish"
    );
    complete_request(&c, &first, request).await;
    assert!(
        c.turn_control(&first.turn_id)
            .await
            .unwrap()
            .steering_pending
    );
    c.finish_chat_turn(&first.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    let continued = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(continued.turn_id, first.turn_id);
    let texts: Vec<_> = continued.history.iter().map(|m| m.visible_text()).collect();
    assert!(texts.ends_with(&["a".into(), "b".into()]));
    assert!(!texts.contains(&"finish".into()));
    assert_eq!(
        c.message(a.message_id.as_deref().unwrap())
            .await
            .unwrap()
            .turn_id,
        first.turn_id
    );
    assert_eq!(c.queued_messages(&session).await.unwrap().len(), 2);
    c.finish_chat_turn(&continued.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    let next = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(Some(&next.turn_id), finish.turn_id.as_ref());
    assert_eq!(next.history.last().unwrap().visible_text(), "finish");
    assert_eq!(c.queued_messages(&session).await.unwrap().len(), 1);
    assert!(
        !c.turn_control(&next.turn_id)
            .await
            .unwrap()
            .steering_pending,
        "a finish message must start alone"
    );
    let request = start_request(&c, &next, "finish").await;
    complete_request(&c, &next, request).await;
    c.finish_chat_turn(&next.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    let continued = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(continued.turn_id, next.turn_id);
    assert_eq!(continued.history.last().unwrap().visible_text(), "later");
    assert!(c.queued_messages(&session).await.unwrap().is_empty());
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_cancels_generation_and_releases_only_the_leading_batch() {
    for finish_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let store = open_test(&dir, None).await;
        let c = store.client();
        let session = c
            .create_session(create_request("interrupt-create", None))
            .await
            .unwrap()
            .session_id;
        send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
        let first = c.claim_next_turn().await.unwrap().unwrap();
        start_request(&c, &first, "cancel").await;
        if finish_first {
            send(&c, &session, "finish-first", DeliveryMode::AfterFinish).await;
        }
        send(&c, &session, "a", DeliveryMode::AfterTurn).await;
        send(&c, &session, "b", DeliveryMode::AfterTurn).await;
        send(&c, &session, "finish", DeliveryMode::AfterFinish).await;
        send(&c, &session, "later", DeliveryMode::AfterTurn).await;
        let interrupt = InterruptSessionRequest {
            command_id: "interrupt-once".into(),
            turn_id: Some(first.turn_id.clone()),
            messages: vec![],
        };
        let receipt = c
            .interrupt_session(&session, interrupt.clone())
            .await
            .unwrap();
        assert!(
            c.turn_control(&first.turn_id)
                .await
                .unwrap()
                .cancellation_requested
        );
        assert!(
            c.claim_next_turn().await.unwrap().is_none(),
            "tools must stop before successor admission"
        );
        c.finish_chat_turn(&first.turn_id, outcome(ChatStatus::Completed))
            .await
            .unwrap();
        assert_eq!(c.turn(&first.turn_id).await.unwrap().status, "cancelled");
        let next = c.claim_next_turn().await.unwrap().unwrap();
        assert_ne!(next.turn_id, first.turn_id);
        let queue = c.queued_messages(&session).await.unwrap();
        if finish_first {
            assert_eq!(next.history.last().unwrap().visible_text(), "finish-first");
            assert_eq!(
                queue.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
                ["a", "b", "finish", "later"]
            );
        } else {
            assert_eq!(
                next.history
                    .iter()
                    .rev()
                    .take(2)
                    .map(|m| m.visible_text())
                    .collect::<Vec<_>>(),
                ["b", "a"]
            );
            assert_eq!(
                queue.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(),
                ["finish", "later"]
            );
        }
        assert_eq!(
            c.interrupt_session(&session, interrupt).await.unwrap(),
            receipt
        );
        assert!(
            !c.turn_control(&next.turn_id)
                .await
                .unwrap()
                .cancellation_requested,
            "retry cannot cancel the successor"
        );
        store.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn queue_survives_restart_and_sending_does_not_require_unpause() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("restart-create", None))
        .await
        .unwrap()
        .session_id;
    let first = send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
    c.pause_turn(
        first.turn_id.as_deref().unwrap(),
        TurnControlRequest {
            command_id: "legacy-pause".into(),
        },
    )
    .await
    .unwrap();
    send(&c, &session, "a", DeliveryMode::AfterTurn).await;
    send(&c, &session, "finish", DeliveryMode::AfterFinish).await;
    assert_eq!(
        c.turn(first.turn_id.as_deref().unwrap())
            .await
            .unwrap()
            .status,
        "queued"
    );
    let running = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(Some(&running.turn_id), first.turn_id.as_ref());
    store.shutdown().await.unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    assert_eq!(
        c.queued_messages(&session)
            .await
            .unwrap()
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["finish"]
    );
    let next = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(next.history.last().unwrap().visible_text(), "a");
    c.finish_chat_turn(&next.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    assert_eq!(
        c.claim_next_turn()
            .await
            .unwrap()
            .unwrap()
            .history
            .last()
            .unwrap()
            .visible_text(),
        "finish"
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupt_accepts_inflight_prompts_once_without_crossing_finish() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("pending-create", None))
        .await
        .unwrap()
        .session_id;
    send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
    let running = c.claim_next_turn().await.unwrap().unwrap();
    let prompt = |text: &str, delivery| SendMessageRequest {
        command_id: format!("pending-{text}"),
        text: text.into(),
        delivery: Some(delivery),
        ..Default::default()
    };
    let accepted = prompt("accepted", DeliveryMode::AfterTurn);
    c.send_message(&session, accepted.clone()).await.unwrap();
    let pending = prompt("pending", DeliveryMode::AfterTurn);
    let messages = vec![
        accepted,
        pending.clone(),
        prompt("finish", DeliveryMode::AfterFinish),
        prompt("later", DeliveryMode::AfterTurn),
        prompt("", DeliveryMode::AfterTurn),
    ];
    c.interrupt_session(
        &session,
        InterruptSessionRequest {
            command_id: "pending-stop".into(),
            turn_id: Some(running.turn_id.clone()),
            messages,
        },
    )
    .await
    .unwrap();
    assert!(
        c.turn_control(&running.turn_id)
            .await
            .unwrap()
            .cancellation_requested
    );
    assert_eq!(
        c.queued_messages(&session).await.unwrap().len(),
        4,
        "invalid input must not prevent cancellation or duplicate earlier input"
    );
    let receipt = c.send_message(&session, pending.clone()).await.unwrap();
    c.finish_chat_turn(&running.turn_id, outcome(ChatStatus::Canceled))
        .await
        .unwrap();
    let next = c.claim_next_turn().await.unwrap().unwrap();
    assert_eq!(
        next.history
            .iter()
            .rev()
            .take(2)
            .map(|m| m.visible_text())
            .collect::<Vec<_>>(),
        ["pending", "accepted"]
    );
    assert_eq!(
        c.queued_messages(&session)
            .await
            .unwrap()
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["finish", "later"]
    );
    assert_eq!(c.send_message(&session, pending).await.unwrap(), receipt);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn after_turn_waits_for_every_proposed_tool_result() {
    use hive_runtime::{agent::ToolIntent, providers::inference::BlockContent, tools::ToolOutcome};
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("tool-queue-create", None))
        .await
        .unwrap()
        .session_id;
    send(&c, &session, "initial", DeliveryMode::AfterTurn).await;
    let running = c.claim_next_turn().await.unwrap().unwrap();
    let request_id = start_request(&c, &running, "tools").await;
    send(&c, &session, "wait-for-tools", DeliveryMode::AfterTurn).await;
    let tools: Vec<_> = (0..2)
        .map(|index| ToolIntent {
            id: format!("tool-{index}"),
            request_id: request_id.clone(),
            result_message_id: format!("tool-result-{index}"),
            call_id: format!("call-{index}"),
            provider_call_id: format!("provider-call-{index}"),
            name: "read".into(),
            arguments: serde_json::json!({"path":"file"}),
        })
        .collect();
    let blocks = tools
        .iter()
        .map(|tool| ContentBlock {
            id: tool.id.clone(),
            content: BlockContent::ToolCall {
                call_id: tool.call_id.clone(),
                provider_call_id: tool.provider_call_id.clone(),
                name: tool.name.clone(),
                arguments: tool.arguments.clone(),
            },
        })
        .collect();
    c.complete_request(
        &running.turn_id,
        RequestCompletion {
            request_id,
            response: InferenceResponse {
                message: InferenceMessage {
                    role: MessageRole::Assistant,
                    blocks,
                    continuation: None,
                },
                finish_reason: FinishReason::ToolCalls,
                resolved_model: None,
                usage: Default::default(),
            },
            tools: tools.clone(),
            compaction: None,
        },
    )
    .await
    .unwrap();
    for (index, tool) in tools.iter().enumerate() {
        assert!(
            !c.turn_control(&running.turn_id)
                .await
                .unwrap()
                .steering_pending
        );
        assert!(
            c.start_tool(&running.turn_id, tool).await.unwrap(),
            "queue must not skip a proposed tool"
        );
        c.finish_tool(
            &running.turn_id,
            tool,
            ToolOutcome {
                status: "completed".into(),
                output: "result".into(),
                artifacts: vec![],
                error_code: None,
                effects_unknown: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            c.turn_control(&running.turn_id)
                .await
                .unwrap()
                .steering_pending,
            index == 1
        );
    }
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn last_user_activity_tracks_acceptance_not_retries_or_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("activity", None))
        .await
        .unwrap()
        .session_id;
    assert_eq!(
        c.session(&session).await.unwrap().last_user_activity_at,
        None
    );
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    send(&c, &session, "first-activity", DeliveryMode::AfterTurn).await;
    let first_at = c
        .session(&session)
        .await
        .unwrap()
        .last_user_activity_at
        .unwrap();
    assert!(first_at >= before);
    // Persisted values are visible through both single-session and list APIs.
    assert_eq!(
        c.sessions(None, 10).await.unwrap().items[0].last_user_activity_at,
        Some(first_at)
    );
    tokio::time::sleep(Duration::from_millis(5)).await;
    send(&c, &session, "first-activity", DeliveryMode::AfterTurn).await;
    assert_eq!(
        c.session(&session).await.unwrap().last_user_activity_at,
        Some(first_at)
    );
    let work = c.claim_next_turn().await.unwrap().unwrap();
    let request = start_request(&c, &work, "activity").await;
    assert_eq!(
        c.session(&session).await.unwrap().last_user_activity_at,
        Some(first_at)
    );
    send(&c, &session, "steer-activity", DeliveryMode::NextBoundary).await;
    let steer_at = c
        .session(&session)
        .await
        .unwrap()
        .last_user_activity_at
        .unwrap();
    assert!(steer_at > first_at);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let queued = send(&c, &session, "queued-activity", DeliveryMode::AfterFinish).await;
    let queued_at = c
        .session(&session)
        .await
        .unwrap()
        .last_user_activity_at
        .unwrap();
    assert!(queued_at > steer_at);
    tokio::time::sleep(Duration::from_millis(5)).await;
    let edit = UpdateQueuedMessageRequest {
        command_id: "activity-edit".into(),
        text: Some("edited input".into()),
        delivery: None,
    };
    c.update_queued_message(&session, queued.message_id.as_ref().unwrap(), edit.clone())
        .await
        .unwrap();
    let edited_at = c
        .session(&session)
        .await
        .unwrap()
        .last_user_activity_at
        .unwrap();
    assert!(edited_at > queued_at);
    tokio::time::sleep(Duration::from_millis(5)).await;
    c.update_queued_message(&session, queued.message_id.as_ref().unwrap(), edit)
        .await
        .unwrap();
    complete_request(&c, &work, request).await;
    assert_eq!(
        c.session(&session).await.unwrap().last_user_activity_at,
        Some(edited_at)
    );
    drop(c);
    store.shutdown().await.unwrap();
    let reopened = open_test(&dir, None).await;
    assert_eq!(
        reopened
            .client()
            .session(&session)
            .await
            .unwrap()
            .last_user_activity_at,
        Some(edited_at)
    );
}

#[tokio::test]
async fn ready_means_completed_and_not_yet_reviewed() {
    use hive_protocol::chat::{SessionStatus, UpdateSessionInboxRequest};
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("ready-create", None))
        .await
        .unwrap()
        .session_id;
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Idle
    );
    send(&c, &session, "ready-first", DeliveryMode::AfterTurn).await;
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Queued
    );
    let first = c.claim_next_turn().await.unwrap().unwrap();
    let running = c.session(&session).await.unwrap();
    assert_eq!(running.status, SessionStatus::Running);
    // Reading while running must not acknowledge the future completion.
    c.update_session_inbox(
        &session,
        UpdateSessionInboxRequest {
            command_id: "read-running".into(),
            read_sequence: Some(running.last_event_sequence),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    c.finish_chat_turn(&first.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    let ready = c.session(&session).await.unwrap();
    assert_eq!(ready.status, SessionStatus::Ready);
    assert_eq!(
        c.sessions(None, 10).await.unwrap().items[0].status,
        SessionStatus::Ready
    );
    // A stale read cursor and unrelated organization updates do not clear ready.
    c.update_session_inbox(
        &session,
        UpdateSessionInboxRequest {
            command_id: "ready-star".into(),
            starred: Some(true),
            read_sequence: Some(running.last_event_sequence),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Ready
    );
    store.shutdown().await.unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let ready = c.session(&session).await.unwrap();
    assert_eq!(ready.status, SessionStatus::Ready);
    let read = UpdateSessionInboxRequest {
        command_id: "review-result".into(),
        read_sequence: Some(ready.last_event_sequence),
        ..Default::default()
    };
    c.update_session_inbox(&session, read.clone())
        .await
        .unwrap();
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Completed
    );
    c.update_session_inbox(
        &session,
        UpdateSessionInboxRequest {
            command_id: "reviewed-archive".into(),
            archived: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Completed
    );
    send(&c, &session, "ready-second", DeliveryMode::AfterTurn).await;
    let second = c.claim_next_turn().await.unwrap().unwrap();
    c.finish_chat_turn(&second.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Ready
    );
    // An idempotent replay of the old acknowledgment cannot hide a new result.
    c.update_session_inbox(&session, read).await.unwrap();
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Ready
    );
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsuccessful_turns_are_not_ready_and_live_work_wins() {
    use hive_protocol::chat::SessionStatus;
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("status-create", None))
        .await
        .unwrap()
        .session_id;
    for (index, status, expected) in [
        (0, ChatStatus::Failed, SessionStatus::Failed),
        (1, ChatStatus::Canceled, SessionStatus::Stopped),
        (2, ChatStatus::Interrupted, SessionStatus::Stopped),
        (3, ChatStatus::Incomplete, SessionStatus::Stopped),
    ] {
        send(
            &c,
            &session,
            &format!("status-{index}"),
            DeliveryMode::AfterTurn,
        )
        .await;
        let turn = c.claim_next_turn().await.unwrap().unwrap();
        c.finish_chat_turn(&turn.turn_id, outcome(status))
            .await
            .unwrap();
        assert_eq!(c.session(&session).await.unwrap().status, expected);
    }
    send(&c, &session, "status-live", DeliveryMode::AfterTurn).await;
    let turn = c.claim_next_turn().await.unwrap().unwrap();
    let request = start_request(&c, &turn, "status-live").await;
    send(&c, &session, "status-next", DeliveryMode::AfterFinish).await;
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Running
    );
    complete_request(&c, &turn, request).await;
    c.finish_chat_turn(&turn.turn_id, outcome(ChatStatus::Completed))
        .await
        .unwrap();
    // A queued successor is not a result awaiting review.
    assert_eq!(
        c.session(&session).await.unwrap().status,
        SessionStatus::Queued
    );
    store.shutdown().await.unwrap();
}
