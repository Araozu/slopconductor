//! Explicitly selected live subscription check: one model-list request and one
//! inference request with no generation-token cap. No credentials are logged.

use std::sync::Arc;

use slop_runtime::providers::{
    ChatMessage, ChatRequest, ProviderClient, TurnOutcome, chatgpt_auth::ChatGptConnection,
    codex::CodexClient,
};

#[tokio::test]
#[ignore = "requires an explicitly selected ChatGPT credential and live usage budget"]
async fn live_subscription_stream_uses_shared_interface() {
    let path = std::env::var("SLOP_CODEX_CREDENTIAL_FILE")
        .expect("live check requires SLOP_CODEX_CREDENTIAL_FILE");
    let connection = Arc::new(
        ChatGptConnection::load(path)
            .await
            .expect("load credential record"),
    );
    let client: Box<dyn ProviderClient> = Box::new(CodexClient::from_chatgpt(connection).unwrap());
    let model = std::env::var("SLOP_CODEX_MODEL").unwrap_or_else(|_| "gpt-6.1-sol".to_owned());
    assert!(client.list_models().await.unwrap().contains(&model));
    let request = ChatRequest {
        model,
        messages: vec![ChatMessage::user("Reply with exactly: ok")],
        max_tokens: None,
        session_id: "slop-codex-live".to_owned(),
    };
    let mut text = String::new();
    let response = client
        .complete_streaming(&request, &mut |delta| text.push_str(&delta.text))
        .await
        .unwrap();
    assert_eq!(response.outcome, TurnOutcome::Completed);
    assert!(!response.text.trim().is_empty());
    assert_eq!(text, response.text);
}
