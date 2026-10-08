//! Opt-in OpenCode Zen checks using the same text-only execution interface.
//!
//! With an explicitly selected `OPENCODE_ZEN_API_KEY` and test budget:
//! ```sh
//! cargo test -p slop-runtime --test opencode_zen_live --locked -- --ignored
//! ```
//! The suite sends one model-list request and six inference requests (blocking
//! and streaming for three wires), each capped at 512 output tokens. Output
//! caps are not billing caps. Missing credentials fail an explicitly chosen run.

use slop_runtime::providers::{
    ChatMessage, ChatRequest, ProviderClient, TurnOutcome, WireProtocol,
    opencode_zen::OpencodeZenClient,
};

fn live_client() -> Box<dyn ProviderClient> {
    Box::new(OpencodeZenClient::from_env().expect("live checks require OPENCODE_ZEN_API_KEY"))
}

async fn check_wire(model: &str, wire: WireProtocol) {
    let client = live_client();
    let request = ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user("Reply with exactly: ok")],
        max_tokens: Some(512),
        session_id: format!("slop-zen-live-{}", model.replace('.', "-")),
    };
    let blocking = client.complete(&request).await.expect("blocking turn");
    assert_eq!(blocking.wire, wire);
    assert_eq!(blocking.model, model);
    assert_eq!(blocking.outcome, TurnOutcome::Completed);
    assert!(!blocking.text.trim().is_empty());
    let mut text = String::new();
    let streaming = client
        .complete_streaming(&request, &mut |delta| text.push_str(&delta.text))
        .await
        .expect("streaming turn");
    assert_eq!(streaming.wire, wire);
    assert_eq!(streaming.model, model);
    assert_eq!(streaming.outcome, TurnOutcome::Completed);
    assert!(!streaming.text.trim().is_empty());
    assert_eq!(text, streaming.text);
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_model_list() {
    let client = live_client();
    let ids = client.list_models().await.expect("model list");
    assert!(!ids.is_empty());
    for model in ["glm-5.3-flash", "gpt-6-luna", "claude-haiku-5-5"] {
        assert!(ids.iter().any(|id| id == model), "missing {model}");
    }
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_chat_completions() {
    check_wire("glm-5.3-flash", WireProtocol::OpenAiChatCompletions).await;
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_responses() {
    check_wire("gpt-6-luna", WireProtocol::OpenAiResponses).await;
}

#[tokio::test]
#[ignore = "requires an explicitly selected live credential and test budget"]
async fn live_messages() {
    check_wire("claude-haiku-5-5", WireProtocol::AnthropicMessages).await;
}
