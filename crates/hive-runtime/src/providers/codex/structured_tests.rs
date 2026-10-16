use std::sync::Arc;

use hive_core::provider::ProviderId;

use super::*;
use crate::providers::{ProviderClient, ProviderError, WireProtocol};
use crate::providers::{
    chatgpt_auth::fixture_connection,
    inference::{
        BlockContent, ContentBlock, FinishReason, GenerationSettings, InferenceMessage,
        InferenceRequest, MessageRole, ProviderEvent,
    },
    test_http::{Reply, Server},
};
use serde_json::{Value, json};

fn request() -> InferenceRequest {
    InferenceRequest {
        request_id: "request-1".into(),
        session_id: "session-1".into(),
        model: "gpt-6.1-sol".into(),
        settings: GenerationSettings {
            max_output_tokens: None,
            reasoning_effort: Some("high".into()),
            full_context: None,
        },
        messages: vec![
            InferenceMessage::text(
                MessageRole::Developer,
                "instruction".into(),
                "Use tools.".into(),
            ),
            InferenceMessage::text(MessageRole::User, "prompt".into(), "Read Cargo.toml".into()),
        ],
        tools: crate::tools::definitions()
            .into_iter()
            .filter(|tool| tool.name == "read")
            .collect(),
    }
}

fn completed() -> Value {
    json!({"status":"completed","model":"resolved-model","output":[
        {"type":"reasoning","id":"reason-1","encrypted_content":"private encrypted fixture","summary":[{"type":"summary_text","text":"private summary"}]},
        {"type":"message","role":"assistant","id":"message-1","phase":"commentary","content":[{"type":"output_text","text":"Inspecting."}]},
        {"type":"function_call","status":"completed","call_id":"read-1","name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}
    ],"usage":{"input_tokens":7,"output_tokens":3}})
}

fn stream(value: Value) -> String {
    format!(
        "data: {}\n\n",
        json!({"type":if value["status"] == "incomplete" {"response.incomplete"} else {"response.completed"},"response":value})
    )
}

fn client(server: &Server, subscription: bool) -> CodexClient {
    if subscription {
        CodexClient::from_chatgpt_with_base_url(
            Arc::new(fixture_connection(std::path::PathBuf::new(), u64::MAX)),
            &server.url,
        )
        .unwrap()
    } else {
        CodexClient::new_with_base_url("synthetic-platform-key", &server.url).unwrap()
    }
}

#[tokio::test]
async fn subscription_sparse_terminal_preserves_completed_items_and_metadata() {
    let provider = CodexClient::from_chatgpt(Arc::new(fixture_connection(
        std::path::PathBuf::new(),
        u64::MAX,
    )))
    .unwrap();
    for tools in [false, true] {
        let output = if tools {
            completed()["output"].as_array().unwrap().clone()
        } else {
            vec![
                json!({"type":"message","status":"completed","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"ok"}]}),
            ]
        };
        let index = usize::from(tools);
        let mut events = vec![
            json!({"type":"response.created","response":{"output":[]}}),
            json!({"type":"response.in_progress","response":{"output":[]}}),
            json!({"type":"response.output_item.added","output_index":index,"item":{"type":"message","content":[]}}),
            json!({"type":"response.content_part.added","output_index":index,"content_index":0,"part":{"type":"output_text","text":""}}),
            json!({"type":"response.output_text.delta","output_index":index,"content_index":0,"delta":if tools {"Inspecting."} else {"ok"}}),
            json!({"type":"response.metadata","metadata":{"private":"not displayed"}}),
        ];
        if tools {
            events.push(json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"Cargo.toml\"}"}));
        }
        for (index, item) in output.iter().enumerate() {
            events
                .push(json!({"type":"response.output_item.done","output_index":index,"item":item}));
        }
        events.push(json!({"type":"response.completed","response":{"status":"completed","output":[],"usage":{"input_tokens":7,"output_tokens":3}}}));
        let body: String = events
            .iter()
            .map(|value| format!("data: {value}\n\n"))
            .collect();
        let mut observed = Vec::new();
        let result = provider
            .read_inference_response(
                &request(),
                WireProtocol::OpenAiResponses,
                crate::providers::test_http::response(&body, None),
                &mut |event| observed.push(event),
            )
            .await
            .unwrap();
        assert_eq!(
            result.message.visible_text(),
            if tools { "Inspecting." } else { "ok" }
        );
        assert_eq!(
            result.finish_reason,
            if tools {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        );
        assert_eq!(result.usage.total_tokens, Some(10));
        assert_eq!(result.message.continuation.unwrap().items, output);
        assert!(
            observed
                .iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta { .. }))
        );
    }
}

#[tokio::test]
async fn subscription_sparse_terminal_requires_consistent_completed_items_and_terminal_evidence() {
    let provider = CodexClient::from_chatgpt(Arc::new(fixture_connection(
        std::path::PathBuf::new(),
        u64::MAX,
    )))
    .unwrap();
    let delta = json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"ok"});
    let done = json!({"type":"response.output_item.done","output_index":0,"item":{"type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"ok"}]}});
    let terminal =
        json!({"type":"response.completed","response":{"status":"completed","output":[]}});
    let mut mismatch = done.clone();
    mismatch["item"]["content"][0]["text"] = "different".into();
    let mut gap = done.clone();
    gap["output_index"] = 1.into();
    for events in [
        vec![delta.clone(), terminal.clone()],
        vec![done.clone()],
        vec![delta.clone(), mismatch, terminal.clone()],
        vec![gap, terminal.clone()],
        vec![done.clone(), done, terminal],
    ] {
        let body: String = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect();
        assert!(
            provider
                .read_inference_response(
                    &request(),
                    WireProtocol::OpenAiResponses,
                    crate::providers::test_http::response(&body, None),
                    &mut |_| {}
                )
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn subscription_structured_stream_accepts_missing_content_type_and_keeps_tools() {
    let provider = CodexClient::from_chatgpt(Arc::new(fixture_connection(
        std::path::PathBuf::new(),
        u64::MAX,
    )))
    .unwrap();
    let body = format!(
        "data: {}\n\n{}",
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Inspecting."}),
        stream(completed())
    );
    for content_type in [
        None,
        Some(""),
        Some("text/plain"),
        Some("text/event-stream"),
    ] {
        let response = crate::providers::test_http::response(&body, content_type);
        let mut events = Vec::new();
        let result = provider
            .read_inference_response(
                &request(),
                WireProtocol::OpenAiResponses,
                response,
                &mut |event| events.push(event),
            )
            .await
            .unwrap();
        assert_eq!(result.finish_reason, FinishReason::ToolCalls);
        assert_eq!(result.message.visible_text(), "Inspecting.");
        assert!(result.message.blocks.iter().any(
            |block| matches!(&block.content, BlockContent::ToolCall { name, .. } if name == "read")
        ));
        assert!(events.iter().any(
            |event| matches!(event, ProviderEvent::TextDelta { text, .. } if text == "Inspecting.")
        ));
        assert_eq!(result.usage.total_tokens, Some(10));
    }
}

#[tokio::test]
async fn subscription_structured_stream_without_content_type_rejects_json_html_and_truncation() {
    let provider = CodexClient::from_chatgpt(Arc::new(fixture_connection(
        std::path::PathBuf::new(),
        u64::MAX,
    )))
    .unwrap();
    for body in [completed().to_string(), "<html>private fixture content</html>".into(), "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".into(), "data: {\"type\":\"response.failed\",\"response\":{\"error\":\"private fixture content\"}}\n\n".into(), "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}".into()] {
        let error = provider.read_inference_response(&request(), WireProtocol::OpenAiResponses, crate::providers::test_http::response(&body, None), &mut |_| {}).await.unwrap_err();
        assert!(!format!("{error:?}").contains("private fixture content"));
    }
}

#[tokio::test]
async fn both_auth_modes_round_trip_tools_and_scoped_private_history() {
    for subscription in [false, true] {
        let final_value = json!({"status":"completed","output":[{"type":"message","role":"assistant","phase":"final_answer","content":[{"type":"output_text","text":"Done."}]}]});
        let first = format!(
            "data: {}\n\ndata: {}\n\n{}",
            json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Inspecting."}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"Cargo.toml\"}"}),
            stream(completed())
        );
        let server = Server::new(vec![Reply::sse(first), Reply::sse(stream(final_value))]).await;
        let concrete = client(&server, subscription);
        let provider: &dyn ProviderClient = &concrete;
        let mut input = request();
        let mut events = Vec::new();
        let response = provider
            .infer(&input, &mut |event| events.push(event))
            .await
            .unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.resolved_model.as_deref(), Some("resolved-model"));
        assert_eq!(response.usage.total_tokens, Some(10));
        assert!(events.iter().any(|e| matches!(
            e,
            ProviderEvent::TextDelta {
                block_index: crate::providers::inference::MAX_BLOCKS,
                ..
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            ProviderEvent::ToolArgumentsDelta {
                    block_index,
                ..
                } if *block_index == 2 * crate::providers::inference::MAX_BLOCKS
        )));
        let continuation = response.message.continuation.as_ref().unwrap();
        assert!(continuation.required);
        assert!(!format!("{continuation:?}").contains("private"));
        let call_id = match &response.message.blocks[1].content {
            BlockContent::ToolCall { call_id, .. } => call_id.clone(),
            _ => panic!("missing completed proposal"),
        };
        let mut incompatible = input.clone();
        let mut other = response.message.clone();
        other.continuation.as_mut().unwrap().scope = "other-registration".into();
        incompatible.messages.push(other);
        let result_message = InferenceMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock {
                id: "result-1".into(),
                content: BlockContent::ToolResult {
                    call_id,
                    provider_call_id: "read-1".into(),
                    is_error: true,
                    output: "Interrupted".into(),
                    artifact_ids: vec!["artifact-1".into()],
                    effects_unknown: true,
                },
            }],
            continuation: None,
        };
        incompatible.messages.push(result_message.clone());
        assert!(matches!(
            provider.infer(&incompatible, &mut |_| {}).await,
            Err(ProviderError::UnsupportedCapability { .. })
        ));
        input.request_id = "request-2".into();
        input.messages.push(response.message);
        input.messages.push(result_message);
        let final_response = provider.infer(&input, &mut |_| {}).await.unwrap();
        assert_eq!(final_response.finish_reason, FinishReason::Stop);
        assert_eq!(final_response.message.visible_text(), "Done.");
        assert_eq!(final_response.usage.total_tokens, None);
        server.inspect(|requests| {
            assert_eq!(requests.len(), 2);
            let bodies: Vec<Value> = requests
                .iter()
                .map(|r| serde_json::from_str(&r.body).unwrap())
                .collect();
            for body in &bodies {
                assert_eq!(body["stream"], true);
                assert_eq!(body["store"], false);
                assert_eq!(body["reasoning"]["effort"], "high");
                assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
                assert!(body.get("max_output_tokens").is_none());
                assert!(body.get("previous_response_id").is_none());
                if subscription {
                    assert!(body.get("tools").is_none());
                    assert_eq!(body["input"][0]["type"], "additional_tools");
                    assert_eq!(body["input"][0]["tools"][0]["name"], "read");
                } else {
                    assert_eq!(body["tools"][0]["name"], "read");
                }
            }
            let history = bodies[1]["input"].as_array().unwrap();
            assert!(
                history
                    .iter()
                    .any(|v| v["encrypted_content"] == "private encrypted fixture")
            );
            assert!(history.iter().any(|v| v["phase"] == "commentary"));
            let result = history
                .iter()
                .find(|v| v["type"] == "function_call_output")
                .unwrap();
            assert_eq!(result["call_id"], "read-1");
            assert!(
                result["output"]
                    .as_str()
                    .unwrap()
                    .contains("effects_unknown")
            );
        });
    }
}

#[tokio::test]
async fn contradictory_streamed_content_or_arguments_never_complete_a_proposal() {
    for event in [
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Different text."}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"different-file\"}"}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{"}),
    ] {
        let body = format!("data: {event}\n\n{}", stream(completed()));
        let server = Server::new(vec![Reply::sse(body)]).await;
        assert!(
            client(&server, true)
                .infer(&request(), &mut |_| {})
                .await
                .is_err()
        );
        server.inspect(|requests| assert_eq!(requests.len(), 1));
    }
}

#[tokio::test]
async fn streamed_refusal_retains_its_canonical_block_and_has_no_tools() {
    let body = format!(
        "data: {}\n\n{}",
        json!({"type":"response.refusal.delta","output_index":0,"content_index":0,"delta":"Cannot do that."}),
        stream(
            json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot do that."}]}]})
        )
    );
    let server = Server::new(vec![Reply::sse(body)]).await;
    let mut events = Vec::new();
    let response = client(&server, true)
        .infer(&request(), &mut |e| events.push(e))
        .await
        .unwrap();
    assert_eq!(response.finish_reason, FinishReason::Refusal);
    assert!(matches!(
        response.message.blocks[0].content,
        BlockContent::Refusal { .. }
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::TextDelta { block_index: 0, .. }))
    );
    let mut follow_up = request();
    let scope = response
        .message
        .continuation
        .as_ref()
        .unwrap()
        .scope
        .clone();
    follow_up.messages.push(response.message);
    let encoded = crate::providers::structured_wire::encode(
        &follow_up,
        WireProtocol::OpenAiResponses,
        ProviderId::Codex,
        &scope,
    )
    .unwrap();
    assert!(encoded["input"].as_array().unwrap().iter().any(|item| {
        item["content"][0]["type"] == "refusal"
            && item["content"][0]["refusal"] == "Cannot do that."
    }));
}

#[tokio::test]
async fn restrictions_fail_before_dispatch_and_model_capabilities_are_specific() {
    let server = Server::new(vec![]).await;
    let provider = client(&server, true);
    for model in MODELS {
        assert!(provider.capabilities(model.id).tools);
    }
    assert!(!provider.capabilities("unknown").tools);
    assert!(
        !provider
            .capabilities("gpt-6.1-sol")
            .reasoning_efforts
            .contains(&"none".into())
    );
    assert!(
        provider
            .capabilities("gpt-6-luna")
            .reasoning_efforts
            .contains(&"none".into())
    );
    for choice in 0..4 {
        let mut input = request();
        match choice {
            0 => input.settings.max_output_tokens = Some(64),
            1 => input.settings.reasoning_effort = Some("ultra".into()),
            2 => input.messages[0].role = MessageRole::System,
            _ => input.model = "unknown".into(),
        }
        assert!(provider.infer(&input, &mut |_| {}).await.is_err());
    }
    server.inspect(|requests| assert!(requests.is_empty()));
}

#[tokio::test]
async fn incomplete_failed_refused_and_malformed_streams_are_distinct() {
    let mut incomplete = completed();
    incomplete["status"] = "incomplete".into();
    incomplete["incomplete_details"] = json!({"reason":"max_output_tokens"});
    incomplete["output"][2]["arguments"] = "{broken".into();
    let mut malformed = completed();
    malformed["output"][2]["arguments"] = "{broken".into();
    let refusal = json!({"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"Declined."}]}]});
    let cases = vec![
        (stream(incomplete), Some(FinishReason::OutputLimit)),
        (stream(malformed), None),
        (stream(refusal), Some(FinishReason::Refusal)),
        ("data: {\"type\":\"response.failed\",\"response\":{\"error\":\"private credential\"}}\n\n".into(), None),
        ("data: {\"type\":\"response.function_call_arguments.delta\",\"delta\":\"{broken\"}\n\n".into(), None),
    ];
    for (body, expected) in cases {
        let server = Server::new(vec![Reply::sse(body)]).await;
        let provider = client(&server, true);
        let result = provider.infer(&request(), &mut |_| {}).await;
        if let Some(expected) = expected {
            let result = result.unwrap();
            assert_eq!(result.finish_reason, expected);
            if expected == FinishReason::OutputLimit {
                assert!(result.message.continuation.is_none());
            }
            assert!(
                !result
                    .message
                    .blocks
                    .iter()
                    .any(|b| matches!(b.content, BlockContent::ToolCall { .. }))
            );
        } else {
            assert!(
                !result
                    .unwrap_err()
                    .to_string()
                    .contains("private credential")
            );
        }
        server.inspect(|requests| assert_eq!(requests.len(), 1));
    }
}

#[tokio::test]
async fn dropping_inference_aborts_the_local_stream_without_retry() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut socket = 'connections: loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut incoming = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let n = socket.read(&mut buffer).await.unwrap();
                assert!(n > 0);
                incoming.extend_from_slice(&buffer[..n]);
                if let Some(end) = incoming.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&incoming[..end]);
                    if headers.starts_with("HEAD / ") {
                        socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        continue 'connections;
                    }
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_owned)
                        })
                        .unwrap()
                        .parse()
                        .unwrap();
                    if incoming.len() >= end + 4 + length {
                        break 'connections socket;
                    }
                }
            }
        };
        let mut buffer = [0; 4096];
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n").await.unwrap();
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 0, "local transport must close after cancellation");
    });
    let provider = CodexClient::new_with_base_url("synthetic-key", &url).unwrap();
    let (seen, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut callback = |event| {
        let _ = seen.send(event);
    };
    let input = request();
    {
        let inference = provider.infer(&input, &mut callback);
        tokio::pin!(inference);
        tokio::select! {
            result = &mut inference => panic!("unexpected completion: {result:?}"),
            event = received.recv() => assert!(matches!(event, Some(ProviderEvent::TextDelta { .. }))),
        }
    }
    server.await.unwrap();
}
