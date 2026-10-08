//! Local HTTP fixtures exercise both concrete adapters through one consumer.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, Response, StatusCode},
};
use tokio::{net::TcpListener, task::JoinHandle};

use super::*;
use crate::providers::{
    ChatMessage, opencode_go::OpencodeGoClient, opencode_zen::OpencodeZenClient,
};

const KEY: &str = "synthetic-header-secret";
const GATEWAYS: [ProviderId; 2] = [ProviderId::OpencodeGo, ProviderId::OpencodeZen];

struct ObservedRequest {
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Value,
}

struct Fixture {
    origin: String,
    requests: Arc<Mutex<Vec<ObservedRequest>>>,
    server: JoinHandle<()>,
}

impl Fixture {
    async fn new(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let app = Router::new().fallback(move |request: Request<Body>| {
            let observed = observed.clone();
            let body = body.clone();
            async move {
                let (parts, input) = request.into_parts();
                let input = to_bytes(input, 4096).await.unwrap();
                observed.lock().unwrap().push(ObservedRequest {
                    method: parts.method,
                    path: parts.uri.path().to_owned(),
                    headers: parts.headers,
                    body: if input.is_empty() {
                        Value::Null
                    } else {
                        serde_json::from_slice(&input).unwrap()
                    },
                });
                Response::builder()
                    .status(status)
                    .header("content-type", content_type)
                    .header("location", "/must-not-follow")
                    .body(Body::from(body))
                    .unwrap()
            }
        });
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            origin,
            requests,
            server,
        }
    }

    fn client(&self, id: ProviderId) -> Box<dyn ProviderClient> {
        let endpoint = format!("{}{}", self.origin, prefix(id));
        match id {
            ProviderId::OpencodeGo => {
                let mut client = OpencodeGoClient::new(KEY).unwrap();
                client.inner.base_url = endpoint;
                Box::new(client)
            }
            ProviderId::OpencodeZen => {
                let mut client = OpencodeZenClient::new(KEY).unwrap();
                client.inner.base_url = endpoint;
                Box::new(client)
            }
            _ => unreachable!(),
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn prefix(id: ProviderId) -> &'static str {
    match id {
        ProviderId::OpencodeGo => "/zen/go/v1",
        ProviderId::OpencodeZen => "/zen/v1",
        _ => unreachable!(),
    }
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![
            ChatMessage {
                role: Role::System,
                content: "system instruction".to_owned(),
            },
            ChatMessage::user("héllo"),
            ChatMessage {
                role: Role::Assistant,
                content: "prior answer".to_owned(),
            },
            ChatMessage::user("next turn"),
        ],
        max_tokens: 64,
        session_id: "fixture-session".to_owned(),
    }
}

fn response(wire: WireProtocol, incomplete: bool) -> Value {
    let mut body = match wire {
        WireProtocol::OpenAiChatCompletions => serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "héllo"},
                "finish_reason": if incomplete { "length" } else { "stop" }}]
        }),
        WireProtocol::OpenAiResponses => serde_json::json!({
            "status": if incomplete { "incomplete" } else { "completed" },
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "héllo"}]}]
        }),
        WireProtocol::AnthropicMessages => serde_json::json!({
            "type": "message", "content": [{"type": "text", "text": "héllo"}],
            "stop_reason": if incomplete { "max_tokens" } else { "end_turn" }
        }),
    };
    body["model"] = "resolved-model".into();
    body["usage"] = serde_json::json!({"input_tokens": 7, "output_tokens": 3});
    body
}

fn sse(wire: WireProtocol, incomplete: bool) -> Vec<u8> {
    let events = match wire {
        WireProtocol::OpenAiChatCompletions => vec![
            serde_json::json!({"model": "resolved-model", "choices": [{"delta": {"content": "héllo"}, "finish_reason": null}]}),
            serde_json::json!({"choices": [{"delta": {}, "finish_reason": if incomplete { "length" } else { "stop" }}], "usage": {"input_tokens": 7, "output_tokens": 3}}),
        ],
        WireProtocol::OpenAiResponses => vec![
            serde_json::json!({"type": "response.output_text.delta", "delta": "héllo"}),
            serde_json::json!({"type": if incomplete { "response.incomplete" } else { "response.completed" }, "response": response(wire, incomplete)}),
        ],
        WireProtocol::AnthropicMessages => vec![
            serde_json::json!({"type": "message_start", "message": {"model": "resolved-model", "content": [], "usage": {"input_tokens": 7}}}),
            serde_json::json!({"type": "content_block_start", "content_block": {"type": "text", "text": ""}}),
            serde_json::json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "héllo"}}),
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": if incomplete { "max_tokens" } else { "end_turn" }}, "usage": {"output_tokens": 3}}),
            serde_json::json!({"type": "message_stop"}),
        ],
    };
    let mut body: String = events
        .iter()
        .map(|event| format!("data: {event}\r\n\r\n"))
        .collect();
    if wire == WireProtocol::OpenAiChatCompletions {
        body.push_str("data: [DONE]\r\n\r\n");
    }
    body.into_bytes()
}

fn assert_inference_request(
    observed: &ObservedRequest,
    id: ProviderId,
    wire: WireProtocol,
    model: &str,
    streaming: bool,
) {
    assert_eq!(observed.method, Method::POST);
    assert_eq!(observed.path, format!("{}{}", prefix(id), wire.path()));
    assert_eq!(observed.headers["user-agent"], USER_AGENT);
    assert_eq!(observed.headers[SESSION_HEADER], "fixture-session");
    assert_eq!(observed.body["model"], model);
    assert!(!observed.body.to_string().contains(KEY));
    assert_eq!(
        observed.body.get("stream"),
        streaming.then_some(&Value::Bool(true))
    );
    if streaming {
        assert_eq!(observed.headers["accept"], "text/event-stream");
    }
    match wire {
        WireProtocol::AnthropicMessages => {
            assert_eq!(observed.headers["x-api-key"], KEY);
            assert_eq!(observed.headers["anthropic-version"], ANTHROPIC_VERSION);
            assert!(!observed.headers.contains_key("authorization"));
            assert_eq!(observed.body["max_tokens"], 64);
            assert_eq!(observed.body["system"], "system instruction");
            assert_eq!(observed.body["messages"].as_array().unwrap().len(), 3);
            assert_eq!(observed.body["messages"][1]["role"], "assistant");
        }
        WireProtocol::OpenAiChatCompletions | WireProtocol::OpenAiResponses => {
            assert_eq!(observed.headers["authorization"], format!("Bearer {KEY}"));
            assert!(!observed.headers.contains_key("x-api-key"));
            let (messages, token_key) = if wire == WireProtocol::OpenAiResponses {
                (&observed.body["input"], "max_output_tokens")
            } else {
                (&observed.body["messages"], "max_tokens")
            };
            assert_eq!(observed.body[token_key], 64);
            assert_eq!(messages.as_array().unwrap().len(), 4);
            assert_eq!(messages[0]["role"], "system");
            assert_eq!(messages[2]["role"], "assistant");
        }
    }
}

#[tokio::test]
async fn both_adapters_use_one_consumer_for_every_wire_and_terminal_outcome() {
    for id in GATEWAYS {
        for (model, wire) in [
            ("glm-5.3-flash", WireProtocol::OpenAiChatCompletions),
            ("gpt-6-luna", WireProtocol::OpenAiResponses),
            ("claude-haiku-5-5", WireProtocol::AnthropicMessages),
        ] {
            for incomplete in [false, true] {
                let body = response(wire, incomplete).to_string().into_bytes();
                let blocking_fixture = Fixture::new(StatusCode::OK, "application/json", body).await;
                let streaming_fixture =
                    Fixture::new(StatusCode::OK, "text/event-stream", sse(wire, incomplete)).await;
                let input = request(model);
                let blocking = blocking_fixture.client(id).complete(&input).await.unwrap();
                let mut text = String::new();
                let streaming = streaming_fixture
                    .client(id)
                    .complete_streaming(&input, &mut |delta| text.push_str(&delta.text))
                    .await
                    .unwrap();
                assert_eq!(streaming, blocking);
                assert_eq!(text, streaming.text);
                assert_eq!(streaming.model, model);
                assert_eq!(streaming.resolved_model.as_deref(), Some("resolved-model"));
                assert_eq!(
                    streaming.usage,
                    Usage::from_reported(Some(7), Some(3), None)
                );
                assert_eq!(streaming.wire, wire);
                assert_eq!(
                    matches!(streaming.outcome, TurnOutcome::Incomplete { .. }),
                    incomplete
                );
                assert_inference_request(
                    &blocking_fixture.requests.lock().unwrap()[0],
                    id,
                    wire,
                    model,
                    false,
                );
                assert_inference_request(
                    &streaming_fixture.requests.lock().unwrap()[0],
                    id,
                    wire,
                    model,
                    true,
                );
            }
        }
    }
}

#[tokio::test]
async fn discovery_preserves_advertised_ids_without_guessing_execution_support() {
    for id in GATEWAYS {
        let fixture = Fixture::new(StatusCode::OK, "application/json", br#"{"data":[{"id":"glm-5.3-flash"},{"id":"gemini-3-flash"},{"id":"unmapped-live-model"}]}"#.to_vec()).await;
        let client = fixture.client(id);
        let models = client.list_models().await.unwrap();
        assert_eq!(
            models,
            ["glm-5.3-flash", "gemini-3-flash", "unmapped-live-model"]
        );
        for model in &models[1..] {
            assert!(
                matches!(client.complete(&request(model)).await, Err(ProviderError::InvalidModel { provider, .. }) if provider == id)
            );
        }
        let requests = fixture.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].path, format!("{}/models", prefix(id)));
        assert_eq!(requests[0].method, Method::GET);
        assert_eq!(
            requests[0].headers["authorization"],
            format!("Bearer {KEY}")
        );
        assert_eq!(requests[0].headers[SESSION_HEADER], "slop-model-discovery");
        assert_eq!(requests[0].headers["user-agent"], USER_AGENT);
    }
}

#[tokio::test]
async fn gateway_specific_models_choose_their_own_wire_not_the_other_catalog() {
    for id in GATEWAYS {
        let wire = if id == ProviderId::OpencodeGo {
            WireProtocol::AnthropicMessages
        } else {
            WireProtocol::OpenAiChatCompletions
        };
        for model in ["minimax-m3", "minimax-m2.7", "qwen3.8-max"] {
            let fixture = Fixture::new(
                StatusCode::OK,
                "application/json",
                response(wire, false).to_string().into_bytes(),
            )
            .await;
            fixture.client(id).complete(&request(model)).await.unwrap();
            assert_inference_request(&fixture.requests.lock().unwrap()[0], id, wire, model, false);
        }
    }
}

#[tokio::test]
async fn streaming_json_fallback_uses_the_same_terminal_validation() {
    for id in GATEWAYS {
        let wire = WireProtocol::OpenAiResponses;
        let fixture = Fixture::new(
            StatusCode::OK,
            "application/json",
            response(wire, false).to_string().into_bytes(),
        )
        .await;
        let mut text = String::new();
        let result = fixture
            .client(id)
            .complete_streaming(&request("gpt-6-luna"), &mut |delta| {
                text.push_str(&delta.text)
            })
            .await
            .unwrap();
        assert_eq!(result.text, text);
        assert_eq!(result.outcome, TurnOutcome::Completed);
    }
}

#[tokio::test]
async fn failures_keep_gateway_identity_and_exclude_reflected_secrets() {
    for id in GATEWAYS {
        for status in [
            StatusCode::UNAUTHORIZED,
            StatusCode::TOO_MANY_REQUESTS,
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::TEMPORARY_REDIRECT,
        ] {
            let fixture = Fixture::new(
                status,
                "application/json",
                format!("reflected {KEY}").into_bytes(),
            )
            .await;
            let error = fixture
                .client(id)
                .complete(&request("glm-5.3-flash"))
                .await
                .unwrap_err();
            assert!(
                matches!(error, ProviderError::UnexpectedStatus { provider, status: code } if provider == id && code == status.as_u16())
            );
            assert!(!error.to_string().contains(KEY));
            assert!(!format!("{error:?}").contains(KEY));
            assert_eq!(
                fixture.requests.lock().unwrap().len(),
                1,
                "no redirects or retries"
            );
        }
        for (content_type, body, failed_turn) in [
            ("application/json", b"not JSON".to_vec(), false),
            ("application/json", br#"{"status":"failed","error":{"message":"synthetic-header-secret"}}"#.to_vec(), true),
            ("text/event-stream", b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n".to_vec(), false),
            ("text/event-stream", b"data: {\"type\":\"response.failed\",\"error\":{\"message\":\"synthetic-header-secret\"}}\n\n".to_vec(), true),
            ("text/event-stream", b"data: \xff\n\n".to_vec(), false),
        ] {
            let fixture = Fixture::new(StatusCode::OK, content_type, body).await;
            let client = fixture.client(id);
            let input = request("gpt-6-luna");
            let error = if content_type == "text/event-stream" {
                client.complete_streaming(&input, &mut |_| {}).await.unwrap_err()
            } else { client.complete(&input).await.unwrap_err() };
            if failed_turn {
                assert!(matches!(error, ProviderError::TurnFailed { provider } if provider == id));
            } else { assert!(matches!(error, ProviderError::InvalidResponse { provider, .. } if provider == id)); }
            assert!(!error.to_string().contains(KEY));
        }
        let fixture = Fixture::new(StatusCode::OK, "application/json", b"{}".to_vec()).await;
        assert!(
            matches!(fixture.client(id).list_models().await, Err(ProviderError::InvalidResponse { provider, .. }) if provider == id)
        );
    }
}
