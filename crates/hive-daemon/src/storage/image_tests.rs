use super::tests::{create_request, open_test};
use super::*;
use hive_protocol::chat::{
    EditMessageRequest, ForkSessionRequest, InterruptSessionRequest, SendMessageRequest,
};
use hive_runtime::{
    agent::RequestIntent,
    chat::{ChatRepository, ChatStatus},
};
use sha2::{Digest, Sha256};

fn png_bytes() -> Vec<u8> {
    [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        .into_iter()
        .chain(b"fake-image".iter().copied())
        .collect()
}

fn png_id() -> String {
    format!("{:x}", Sha256::digest(png_bytes()))
}

async fn register_png(c: &StoreClient) -> String {
    let bytes = png_bytes();
    let id = png_id();
    let metadata = c
        .register_artifact(&id, bytes.len() as u64, "image/png")
        .await
        .unwrap();
    assert_eq!(metadata.id, id);
    assert_eq!(metadata.media_type, "image/png");
    id
}

fn image_request(command_id: &str, text: &str, id: &str) -> SendMessageRequest {
    SendMessageRequest {
        command_id: command_id.into(),
        text: text.into(),
        images: vec![hive_protocol::chat::ImageAttachment {
            artifact_id: id.into(),
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn image_send_accepted_and_history_shows_text_plus_image_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    let session = c
        .create_session(create_request("image-create", None))
        .await
        .unwrap()
        .session_id;
    let receipt = c
        .send_message(&session, image_request("image-send", "Look at this", &id))
        .await
        .unwrap();
    let message_id = receipt.message_id.unwrap();
    let history = c.messages(&session, None, 10).await.unwrap();
    let message = history
        .items
        .iter()
        .find(|m| m.id == message_id)
        .expect("sent message in history");
    assert_eq!(message.text, "Look at this");
    assert_eq!(message.blocks.len(), 2);
    assert!(matches!(
        &message.blocks[0].content,
        hive_protocol::execution::BlockContent::Text { text } if text == "Look at this"
    ));
    assert!(matches!(
        &message.blocks[1].content,
        hive_protocol::execution::BlockContent::Image { artifact_id, media_type }
        if artifact_id == &id && media_type == "image/png"
    ));
    // The canonical message lookup exposes the same blocks.
    let canonical = c.message(&message_id).await.unwrap();
    assert_eq!(canonical.blocks.len(), 2);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn empty_text_with_images_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    let session = c
        .create_session(create_request("image-empty-create", None))
        .await
        .unwrap()
        .session_id;
    let receipt = c
        .send_message(&session, image_request("image-empty", "", &id))
        .await
        .unwrap();
    let message = c.message(&receipt.message_id.unwrap()).await.unwrap();
    assert_eq!(message.blocks.len(), 2);
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn image_send_to_text_only_model_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    // Session-level non-vision model.
    let mut visionless = create_request("image-novision-create", None);
    visionless.model = "hy3".into();
    let session = c.create_session(visionless).await.unwrap().session_id;
    assert!(matches!(
        c.send_message(&session, image_request("image-novision", "See", &id))
            .await,
        Err(StoreError::Unsupported(_))
    ));
    // Per-message model override to a non-vision model is also rejected.
    let vision = c
        .create_session(create_request("image-override-create", None))
        .await
        .unwrap()
        .session_id;
    let mut request = image_request("image-override", "See", &id);
    request.model = Some("opencode-go/hy3".into());
    assert!(matches!(
        c.send_message(&vision, request).await,
        Err(StoreError::Unsupported(_))
    ));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_or_malformed_image_artifacts_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let session = c
        .create_session(create_request("image-bad-create", None))
        .await
        .unwrap()
        .session_id;
    // Well-formed but unregistered id.
    assert!(matches!(
        c.send_message(&session, image_request("image-unknown", "See", &png_id()))
            .await,
        Err(StoreError::NotFound)
    ));
    // Malformed id.
    let mut malformed = image_request("image-malformed", "See", "not-hex");
    malformed.images[0].artifact_id = "not-hex".into();
    assert!(matches!(
        c.send_message(&session, malformed).await,
        Err(StoreError::Invalid)
    ));
    // Registered non-image artifact.
    let text_id = format!("{:x}", Sha256::digest(b"plain text artifact"));
    c.register_artifact(&text_id, 19, "application/json")
        .await
        .unwrap();
    assert!(matches!(
        c.send_message(&session, image_request("image-wrong-type", "See", &text_id))
            .await,
        Err(StoreError::Invalid)
    ));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn command_replay_with_different_images_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    let other_bytes: Vec<u8> = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        .into_iter()
        .chain(b"other-image".iter().copied())
        .collect();
    let other = format!("{:x}", Sha256::digest(&other_bytes));
    c.register_artifact(&other, other_bytes.len() as u64, "image/png")
        .await
        .unwrap();
    let session = c
        .create_session(create_request("image-replay-create", None))
        .await
        .unwrap()
        .session_id;
    let first = c
        .send_message(&session, image_request("image-replay", "See", &id))
        .await
        .unwrap();
    // Identical retry returns the original receipt.
    assert_eq!(
        c.send_message(&session, image_request("image-replay", "See", &id))
            .await
            .unwrap(),
        first
    );
    // Same command id with different images conflicts.
    assert!(matches!(
        c.send_message(&session, image_request("image-replay", "See", &other))
            .await,
        Err(StoreError::Conflict)
    ));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_images_dispatch_as_blocks_and_patch_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    let session = c
        .create_session(create_request("image-queue-create", None))
        .await
        .unwrap()
        .session_id;
    c.send_message(
        &session,
        SendMessageRequest {
            command_id: "image-queue-first".into(),
            text: "initial".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let first = c.claim_next_turn().await.unwrap().unwrap();
    assert!(
        c.begin_request(
            &first.turn_id,
            RequestIntent {
                id: "image-queue-request".into(),
                message_id: "image-queue-assistant".into(),
                ordinal: first.next_request_ordinal,
                requested_model: first.requested_model.clone(),
                requested_settings: first.requested_settings.clone(),
                settings: first.settings.clone(),
                context: Default::default(),
            }
        )
        .await
        .unwrap()
    );
    // Queue an image message behind the running turn.
    let mut queued = image_request("image-queued", "queued look", &id);
    queued.delivery = Some(hive_protocol::chat::DeliveryMode::AfterTurn);
    let queued_id = c
        .send_message(&session, queued)
        .await
        .unwrap()
        .message_id
        .unwrap();
    let entries = c.queued_messages(&session).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].text, "queued look");
    assert_eq!(entries[0].images.len(), 1);
    assert_eq!(entries[0].images[0].artifact_id, id);
    // A text edit keeps the images.
    c.update_queued_message(
        &session,
        &queued_id,
        hive_protocol::chat::UpdateQueuedMessageRequest {
            command_id: "image-queue-edit".into(),
            text: Some("edited look".into()),
            delivery: None,
        },
    )
    .await
    .unwrap();
    let entries = c.queued_messages(&session).await.unwrap();
    assert_eq!(entries[0].text, "edited look");
    assert_eq!(entries[0].images.len(), 1);
    // Interrupt the running turn; the queued image message dispatches with blocks.
    c.interrupt_session(
        &session,
        InterruptSessionRequest {
            command_id: "image-queue-interrupt".into(),
            turn_id: None,
            messages: vec![],
        },
    )
    .await
    .unwrap();
    c.finish_chat_turn(
        &first.turn_id,
        hive_runtime::chat::ChatOutcome {
            text: String::new(),
            resolved_model: None,
            usage: Default::default(),
            status: ChatStatus::Canceled,
            error_code: None,
            error_message: None,
        },
    )
    .await
    .unwrap();
    let next = c.claim_next_turn().await.unwrap().unwrap();
    let dispatched = next
        .history
        .iter()
        .find(|m| {
            m.blocks.iter().any(|b| {
                matches!(
                    &b.content,
                    hive_runtime::providers::inference::BlockContent::Image {
                        artifact_id,
                        ..
                    } if artifact_id == &id
                )
            })
        })
        .expect("dispatched image message in inference history");
    assert_eq!(dispatched.visible_text(), "edited look");
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_copies_image_blocks_and_edit_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_test(&dir, None).await;
    let c = store.client();
    let id = register_png(&c).await;
    let session = c
        .create_session(create_request("image-fork-create", None))
        .await
        .unwrap()
        .session_id;
    let message = c
        .send_message(&session, image_request("image-fork-send", "Fork me", &id))
        .await
        .unwrap()
        .message_id
        .unwrap();
    let forked = c
        .fork_session(
            &session,
            ForkSessionRequest {
                command_id: "image-fork".into(),
                message_id: message.clone(),
                title: None,
                expected_revision: None,
            },
        )
        .await
        .unwrap()
        .session_id;
    let history = c.messages(&forked, None, 10).await.unwrap();
    let copied = history
        .items
        .iter()
        .find(|m| m.role == "user")
        .expect("forked user message");
    assert!(copied.blocks.iter().any(|b| matches!(
        &b.content,
        hive_protocol::execution::BlockContent::Image { artifact_id, .. }
        if artifact_id == &id
    )));
    // Editing the user message keeps its images with the replacement text.
    let edited = c
        .edit_message(
            &session,
            &message,
            EditMessageRequest {
                command_id: "image-edit".into(),
                text: "Revised prompt".into(),
                expected_revision: None,
            },
            None,
        )
        .await
        .unwrap()
        .session_id;
    let history = c.messages(&edited, None, 10).await.unwrap();
    let replacement = history
        .items
        .iter()
        .find(|m| m.role == "user" && m.text == "Revised prompt")
        .expect("edited user message");
    assert!(replacement.blocks.iter().any(|b| matches!(
        &b.content,
        hive_protocol::execution::BlockContent::Image { artifact_id, .. }
        if artifact_id == &id
    )));
    store.shutdown().await.unwrap();
}
