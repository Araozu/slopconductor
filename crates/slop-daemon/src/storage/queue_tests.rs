use super::tests::{create_request, open_test};
use super::*;
use slop_protocol::chat::InterruptSessionRequest;
use slop_runtime::{
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
                        content: slop_runtime::providers::inference::BlockContent::Text {
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
    use slop_runtime::{agent::ToolIntent, providers::inference::BlockContent, tools::ToolOutcome};
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
