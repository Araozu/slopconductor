//! Explicitly selected live subscription checks. The text check uses one catalog
//! and one inference request; the tool check uses one catalog and two inference
//! requests, with a synthetic read result and no local side effects or token cap.
//! No credentials are logged.

use std::sync::Arc;

use hive_runtime::providers::inference::{
    BlockContent, ContentBlock, FinishReason, GenerationSettings, InferenceMessage,
    InferenceRequest, MessageRole, ToolDefinition,
};
use hive_runtime::providers::{
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

#[tokio::test]
#[ignore = "requires an explicitly selected ChatGPT credential and two-inference live usage budget"]
async fn live_subscription_tool_result_round_trip() {
    let path = std::env::var("SLOP_CODEX_CREDENTIAL_FILE")
        .expect("live check requires SLOP_CODEX_CREDENTIAL_FILE");
    let connection = Arc::new(
        ChatGptConnection::load(path)
            .await
            .expect("load credential record"),
    );
    let client = CodexClient::from_chatgpt(connection).unwrap();
    let model = std::env::var("SLOP_CODEX_MODEL").unwrap_or_else(|_| "gpt-6.1-sol".to_owned());
    assert!(client.list_models().await.unwrap().contains(&model));
    let mut request = InferenceRequest {
        request_id: "codex-live-tool-1".into(),
        session_id: "codex-live-tool".into(),
        model,
        settings: GenerationSettings::default(),
        messages: vec![InferenceMessage::text(MessageRole::User, "live-prompt".into(),
            "Call inspect_fixture once to read the synthetic Cargo manifest. After its result, state the package name. Do not guess it.".into())],
        tools: vec![ToolDefinition {
            name: "inspect_fixture".into(),
            description: "Read the synthetic Cargo manifest fixture; takes no arguments.".into(),
            parameters: serde_json::json!({"type":"object","properties":{},"required":[],"additionalProperties":false}),
        }],
        images: Default::default(),
    };
    let response = client.infer(&request, &mut |_| {}).await.unwrap();
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    let calls: Vec<_> = response
        .message
        .blocks
        .iter()
        .filter_map(|block| match &block.content {
            BlockContent::ToolCall {
                call_id,
                provider_call_id,
                name,
                arguments,
            } => {
                assert_eq!(name, "inspect_fixture");
                assert_eq!(arguments, &serde_json::json!({}));
                Some((call_id.clone(), provider_call_id.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 1);
    let (call_id, provider_call_id) = calls.into_iter().next().unwrap();
    request.messages.push(response.message);
    request.messages.push(InferenceMessage {
        role: MessageRole::Tool,
        continuation: None,
        blocks: vec![ContentBlock {
            id: "live-result".into(),
            content: BlockContent::ToolResult {
                call_id,
                provider_call_id,
                is_error: false,
                output: "[package]\nname = \"slop-codex-fixture\"\nversion = \"0.1.0\"\n".into(),
                artifact_ids: vec![],
                effects_unknown: false,
            },
        }],
    });
    request.request_id = "codex-live-tool-2".into();
    request.tools.clear();
    let response = client.infer(&request, &mut |_| {}).await.unwrap();
    assert_eq!(response.finish_reason, FinishReason::Stop);
    assert!(
        response
            .message
            .visible_text()
            .contains("slop-codex-fixture")
    );
}
