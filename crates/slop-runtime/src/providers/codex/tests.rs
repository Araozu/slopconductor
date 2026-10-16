use std::sync::Arc;

use super::super::{
    ChatMessage, ChatRequest, ProviderClient, ProviderError, ProviderId, TurnOutcome, UsageSource,
    chatgpt_auth::fixture_connection,
    test_http::{Reply, Server},
};
use super::*;

fn request() -> ChatRequest {
    ChatRequest {
        model: "gpt-6.1-sol".to_owned(),
        messages: vec![
            ChatMessage {
                role: super::super::Role::Developer,
                content: "fixture instruction".to_owned(),
            },
            ChatMessage::user("fixture prompt"),
        ],
        max_tokens: None,
        session_id: "fixture-session".to_owned(),
    }
}

fn completed(text: &str) -> serde_json::Value {
    serde_json::json!({"status":"completed","model":"fixture-resolved-model","output":[{"type":"message","content":[{"type":"output_text","text":text}]}],"usage":{"input_tokens":5,"output_tokens":2}})
}

fn stream() -> String {
    format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"héllo\"}}\n\ndata: {}\n\n",
        serde_json::json!({"type":"response.completed","response":completed("héllo")})
    )
}

fn subscription(server: &Server) -> CodexClient {
    let connection = Arc::new(fixture_connection(std::path::PathBuf::new(), u64::MAX));
    let mut client = CodexClient::from_chatgpt(connection).unwrap();
    client.base_url = server.url.clone();
    client
}

#[tokio::test]
async fn subscription_discovery_and_both_completion_modes_use_shared_surface() {
    let server = Server::new(vec![Reply::json(serde_json::json!({"models":[{"slug":"gpt-6.1-sol","visibility":"list"},{"slug":"hidden-model","visibility":"hide"}]})), Reply::sse(stream()), Reply::sse(stream())]).await;
    let concrete = subscription(&server);
    let client: &dyn ProviderClient = &concrete;
    assert_eq!(client.list_models().await.unwrap(), vec!["gpt-6.1-sol"]);
    let first = client.complete(&request()).await.unwrap();
    let mut text = String::new();
    let second = client
        .complete_streaming(&request(), &mut |delta| text.push_str(&delta.text))
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(text, "héllo");
    assert_eq!(first.outcome, TurnOutcome::Completed);
    assert_eq!(
        first.resolved_model.as_deref(),
        Some("fixture-resolved-model")
    );
    assert_eq!(first.usage.total_tokens, Some(7));
    assert_eq!(first.usage.total_source, Some(UsageSource::Derived));
    server.inspect(|requests| {
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0].path, "/models");
        for sent in requests {
            assert_eq!(sent.headers["authorization"], "Bearer fixture-access-token");
            assert!(
                sent.headers["user-agent"]
                    .to_str()
                    .unwrap()
                    .starts_with("hive/")
            );
            assert!(!sent.headers.contains_key("x-opencode-session"));
            assert!(!sent.body.contains("fixture-access-token"));
            if sent.path == "/responses" {
                let body: serde_json::Value = serde_json::from_str(&sent.body).unwrap();
                assert_eq!(body["store"], false);
                assert_eq!(body["stream"], true);
                assert!(body.get("max_output_tokens").is_none());
                assert_eq!(body["input"][0]["role"], "developer");
                assert_eq!(body["input"][1]["role"], "user");
                assert_eq!(body["prompt_cache_key"], "fixture-session");
            }
        }
    });
}

#[tokio::test]
async fn subscription_rejects_ignored_stream_setting() {
    let server = Server::new(vec![Reply::json(completed("unexpected JSON"))]).await;
    let client = subscription(&server);
    assert!(matches!(
        client.complete(&request()).await,
        Err(ProviderError::InvalidResponse {
            provider: ProviderId::Codex,
            ..
        })
    ));
    server.inspect(|requests| assert_eq!(requests.len(), 1));
}

#[tokio::test]
async fn unsupported_configuration_fails_before_network_or_deltas() {
    let server = Server::new(vec![]).await;
    let client = subscription(&server);
    for invalid in [
        ChatRequest {
            max_tokens: Some(64),
            ..request()
        },
        ChatRequest {
            model: "unknown-model".to_owned(),
            ..request()
        },
        ChatRequest {
            messages: vec![ChatMessage {
                role: super::super::Role::System,
                content: "instruction".to_owned(),
            }],
            ..request()
        },
    ] {
        assert!(client.complete(&invalid).await.is_err());
        let mut emitted = false;
        assert!(
            client
                .complete_streaming(&invalid, |_| emitted = true)
                .await
                .is_err()
        );
        assert!(!emitted);
    }
    server.inspect(|requests| assert!(requests.is_empty()));
}

#[tokio::test]
async fn incomplete_failed_truncated_and_structured_turns_do_not_become_success() {
    let mut incomplete = completed("partial");
    incomplete["status"] = "incomplete".into();
    incomplete["incomplete_details"] = serde_json::json!({"reason":"max_output_tokens"});
    let cases = [
            (format!("data: {}\n\n",serde_json::json!({"type":"response.incomplete","response":incomplete})), true),
            ("data: {\"type\":\"response.failed\",\"response\":{\"error\":\"fixture-access-token\"}}\n\n".to_owned(), false),
            ("data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_owned(), false),
            ("data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\"}}\n\n".to_owned(), false),
        ];
    for (body, preserves_partial) in cases {
        let server = Server::new(vec![Reply::sse(body)]).await;
        let client = subscription(&server);
        let result = client.complete(&request()).await;
        if preserves_partial {
            let result = result.unwrap();
            assert_eq!(result.text, "partial");
            assert!(matches!(result.outcome, TurnOutcome::Incomplete { .. }));
        } else {
            let error = result.unwrap_err();
            assert!(!error.to_string().contains("fixture-access-token"));
            if let ProviderError::InvalidResponse { provider, .. }
            | ProviderError::TurnFailed { provider } = error
            {
                assert_eq!(provider, ProviderId::Codex);
            }
        }
        server.inspect(|requests| assert_eq!(requests.len(), 1));
    }
}

#[tokio::test]
async fn api_key_path_preserves_cap_and_roles_and_redirects_are_not_followed() {
    let destination = Server::new(vec![Reply::json(completed("unexpected redirect"))]).await;
    let server = Server::new(vec![
        Reply::json(completed("ok")),
        Reply {
            status: axum::http::StatusCode::TEMPORARY_REDIRECT,
            content_type: "application/json",
            body: "fixture-key reflected secret".to_owned(),
            location: Some(destination.url.clone()),
        },
    ])
    .await;
    let mut concrete = CodexClient::new("fixture-key").unwrap();
    concrete.base_url = server.url.clone();
    let client: &dyn ProviderClient = &concrete;
    let mut input = request();
    input.max_tokens = Some(64);
    input.messages.insert(
        0,
        ChatMessage {
            role: super::super::Role::System,
            content: "leading instruction".to_owned(),
        },
    );
    assert_eq!(client.complete(&input).await.unwrap().text, "ok");
    let error = client.complete(&input).await.unwrap_err();
    assert!(matches!(
        error,
        ProviderError::UnexpectedStatus {
            provider: ProviderId::Codex,
            status: 307
        }
    ));
    assert!(!error.to_string().contains("fixture-key"));
    server.inspect(|requests| {
        assert_eq!(requests.len(), 2);
        let body: serde_json::Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["max_output_tokens"], 64);
        assert_eq!(body["stream"], false);
        assert_eq!(body["input"][0]["role"], "system");
    });
    destination.inspect(|requests| assert!(requests.is_empty()));
    assert!(CodexClient::new("  ").is_err());
}
