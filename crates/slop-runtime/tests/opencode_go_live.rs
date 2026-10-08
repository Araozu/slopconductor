//! Live OpenCode Go checks. Run with `OPENCODE_GO_API_KEY` set:
//!
//! ```sh
//! set -a; source .env; set +a
//! cargo test -p slop-runtime --test opencode_go_live --locked -- --ignored
//! ```
//!
//! Ignored by default: choose credentials and a live test budget explicitly.
//! The full suite makes one model-list request and four inference requests with
//! output caps of 512, 64, 512, and 512 tokens; these are not billing caps.
//! An explicitly selected live run fails if its credential is missing.

use slop_runtime::providers::{
    ChatMessage, ChatRequest, ProviderClient, opencode_go::OpencodeGoClient,
};

fn live_client() -> Box<dyn ProviderClient> {
    Box::new(OpencodeGoClient::from_env().expect("live checks require OPENCODE_GO_API_KEY"))
}

fn request(model: &str, text: &str, max_tokens: u32) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user(text)],
        max_tokens: Some(max_tokens),
        session_id: format!("slop-live-{}", model.replace('.', "-")),
    }
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_model_list_contains_catalog_ids() {
    let client = live_client();
    let ids = client.list_models().await.expect("list_models");
    assert!(!ids.is_empty());
    for known in ["glm-5.3-flash", "claude-haiku-5-5", "gpt-6-luna"] {
        assert!(
            ids.iter().any(|id| id == known),
            "live list is missing documented model {known}"
        );
    }
    // Every catalog id must resolve to its documented wire shape.
    for model in client.descriptor().models() {
        assert!(client.descriptor().wire_protocol(model.id).is_ok());
    }
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_chat_completions_turn() {
    let client = live_client();
    // Reasoning models can spend a small budget on thinking before producing
    // visible text; 512 tokens leaves room for both.
    let response = client
        .complete(&request("glm-5.3-flash", "Reply with exactly: ok", 512))
        .await
        .expect("chat completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens.is_some_and(|tokens| tokens > 0));
    assert_eq!(
        response.outcome,
        slop_runtime::providers::TurnOutcome::Completed
    );
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_anthropic_messages_turn() {
    let client = live_client();
    let response = client
        .complete(&request("claude-haiku-5-5", "Reply with exactly: ok", 64))
        .await
        .expect("messages completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens.is_some_and(|tokens| tokens > 0));
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_responses_turn() {
    let client = live_client();
    let response = client
        .complete(&request("gpt-6-luna", "Reply with exactly: ok", 512))
        .await
        .expect("responses completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens.is_some_and(|tokens| tokens > 0));
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_chat_streaming_assembles_text() {
    let client = live_client();
    let mut deltas = 0;
    let response = client
        .complete_streaming(
            &request("glm-5.3-flash", "Reply with exactly: ok", 512),
            &mut |_| deltas += 1,
        )
        .await
        .expect("streaming completion");
    assert!(deltas > 0, "expected at least one SSE delta");
    assert!(!response.text.trim().is_empty());
}
