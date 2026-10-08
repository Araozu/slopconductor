//! Live OpenCode Go checks. Run with `OPENCODE_GO_API_KEY` set:
//!
//! ```sh
//! set -a; source .env; set +a
//! cargo test -p slop-runtime --test opencode_go_live --locked
//! ```
//!
//! Without the key every test prints a skip notice and passes, so CI without
//! credentials stays green. With the key, three tiny bounded calls verify the
//! documented wire shapes end to end (list, chat, messages, responses).

use slop_runtime::providers::{
    ChatMessage, ChatRequest, OpencodeGoProvider, Provider as _, opencode_go::OpencodeGoClient,
};

fn live_client() -> Option<OpencodeGoClient> {
    match std::env::var("OPENCODE_GO_API_KEY") {
        Ok(key) if !key.trim().is_empty() => {
            Some(OpencodeGoClient::new(&key).expect("OPENCODE_GO_API_KEY must build a client"))
        }
        _ => {
            eprintln!("skipping live OpenCode Go test: OPENCODE_GO_API_KEY not set");
            None
        }
    }
}

fn request(model: &str, text: &str, max_tokens: u32) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user(text)],
        max_tokens,
        session_id: format!("slop-live-{}", model.replace('.', "-")),
    }
}

#[tokio::test]
async fn live_model_list_contains_catalog_ids() {
    let Some(client) = live_client() else {
        return;
    };
    let ids = client.list_models().await.expect("list_models");
    assert!(!ids.is_empty());
    for known in ["glm-5.3-flash", "claude-haiku-5-5", "gpt-6-luna"] {
        assert!(
            ids.iter().any(|id| id == known),
            "live list is missing documented model {known}"
        );
    }
    // Every catalog id must resolve to its documented wire shape.
    for model in OpencodeGoProvider.models() {
        assert!(OpencodeGoProvider.wire_protocol(model.id).is_ok());
    }
}

#[tokio::test]
async fn live_chat_completions_turn() {
    let Some(client) = live_client() else {
        return;
    };
    // Reasoning models can spend a small budget on thinking before producing
    // visible text; 512 tokens leaves room for both.
    let response = client
        .complete(&request("glm-5.3-flash", "Reply with exactly: ok", 512))
        .await
        .expect("chat completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens > 0);
    assert_eq!(
        response.outcome,
        slop_runtime::providers::TurnOutcome::Completed
    );
}

#[tokio::test]
async fn live_anthropic_messages_turn() {
    let Some(client) = live_client() else {
        return;
    };
    let response = client
        .complete(&request("claude-haiku-5-5", "Reply with exactly: ok", 64))
        .await
        .expect("messages completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens > 0);
}

#[tokio::test]
async fn live_responses_turn() {
    let Some(client) = live_client() else {
        return;
    };
    let response = client
        .complete(&request("gpt-6-luna", "Reply with exactly: ok", 512))
        .await
        .expect("responses completion");
    assert!(!response.text.trim().is_empty());
    assert!(response.usage.total_tokens > 0);
}

#[tokio::test]
async fn live_chat_streaming_assembles_text() {
    let Some(client) = live_client() else {
        return;
    };
    let mut deltas = 0;
    let response = client
        .complete_streaming(
            &request("glm-5.3-flash", "Reply with exactly: ok", 512),
            |_| deltas += 1,
        )
        .await
        .expect("streaming completion");
    assert!(deltas > 0, "expected at least one SSE delta");
    assert!(!response.text.trim().is_empty());
}
