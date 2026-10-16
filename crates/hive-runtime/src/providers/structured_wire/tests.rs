use hive_core::provider::ProviderId;
use serde_json::{Value, json};

use super::{decoding::decode, encoding::encode, fold::StructuredFold};
use crate::providers::{
    WireProtocol,
    inference::{self, *},
    opencode::SseStream,
    opencode_go::OpencodeGoProvider,
};

#[test]
fn plan_usage_stream_errors_keep_known_codes_without_upstream_messages() {
    for code in [
        "subscription_sharing_user_not_eligible",
        "subscription_sharing_usage_limit_exceeded",
        "subscription_sharing_usage_unavailable",
        "subscription_sharing_unsupported_capability",
        "subscription_sharing_route_not_supported",
    ] {
        let event = format!(
            "data: {}",
            json!({"type":"response.failed", "response":{"error":{"code":code,"message":"private-credential-reflected"}}})
        );
        let error = StructuredFold::new(WireProtocol::OpenAiResponses)
            .feed(&event, &mut |_| {})
            .unwrap_err()
            .for_provider(ProviderId::Codex);
        assert!(
            matches!(error, crate::providers::ProviderError::PlanUsage { code: saved } if saved == code)
        );
        assert!(!format!("{error:?}").contains("private-credential-reflected"));
    }
    let event = format!(
        "data: {}",
        json!({"type":"response.failed", "response":{"error":{"code":"private-credential-reflected", "message":"private-credential-reflected"}}})
    );
    let error = StructuredFold::new(WireProtocol::OpenAiResponses)
        .feed(&event, &mut |_| {})
        .unwrap_err()
        .for_provider(ProviderId::Codex);
    assert!(matches!(
        error,
        crate::providers::ProviderError::TurnFailed { .. }
    ));
    assert!(!format!("{error:?}").contains("private-credential-reflected"));
}

fn request(wire: WireProtocol) -> InferenceRequest {
    InferenceRequest {
        request_id: "request-1".into(),
        session_id: "session-1".into(),
        model: match wire {
            WireProtocol::OpenAiChatCompletions => "glm-5.3-flash",
            WireProtocol::OpenAiResponses => "gpt-6-luna",
            WireProtocol::AnthropicMessages => "minimax-m3",
        }
        .into(),
        settings: GenerationSettings {
            max_output_tokens: Some(512),
            reasoning_effort: None,
            full_context: None,
        },
        messages: vec![InferenceMessage::text(
            MessageRole::User,
            "user-block".into(),
            "Read Cargo.toml".into(),
        )],
        tools: crate::tools::definitions()
            .into_iter()
            .filter(|d| d.name == "read")
            .collect(),
    }
}
fn terminal(wire: WireProtocol) -> Value {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            json!({"model":"resolved","choices":[{"finish_reason":"tool_calls","message":{"content":"Inspecting.","reasoning_content":"private thinking","tool_calls":[{"id":"provider-call","type":"function","function":{"name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}}]}}],"usage":{"prompt_tokens":7,"completion_tokens":3}})
        }
        WireProtocol::OpenAiResponses => {
            json!({"model":"resolved","status":"completed","output":[{"id":"reason","type":"reasoning","encrypted_content":"private opaque"},{"type":"message","content":[{"type":"output_text","text":"Inspecting."}]},{"type":"function_call","status":"completed","call_id":"provider-call","name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}],"usage":{"input_tokens":7,"output_tokens":3}})
        }
        WireProtocol::AnthropicMessages => {
            json!({"model":"resolved","stop_reason":"tool_use","content":[{"type":"thinking","thinking":"private thinking","signature":"signed"},{"type":"text","text":"Inspecting."},{"type":"tool_use","id":"provider-call","name":"read","input":{"path":"Cargo.toml"}}],"usage":{"input_tokens":7,"output_tokens":3}})
        }
    }
}

#[test]
fn gateway_efforts_are_validated_and_encoded_for_each_wire() {
    use crate::providers::Provider;
    let provider = OpencodeGoProvider::instance();
    for (model, expected) in [
        ("glm-5.3-flash", json!({"reasoning_effort":"max"})),
        ("gpt-6-luna", json!({"reasoning":{"effort":"max"}})),
        (
            "claude-haiku-5-5",
            json!({"output_config":{"effort":"max"}}),
        ),
    ] {
        let wire = provider.wire_protocol(model).unwrap();
        let mut input = request(wire);
        input.model = model.into();
        input.settings.reasoning_effort = Some("max".into());
        let caps = inference::capabilities(provider, model);
        input.validate(provider, &caps).unwrap();
        let encoded = encode(&input, wire, ProviderId::OpencodeGo, "scope").unwrap();
        for (key, value) in expected.as_object().unwrap() {
            assert_eq!(&encoded[key], value, "{model}");
        }
        input.settings.reasoning_effort = None;
        let encoded = encode(&input, wire, ProviderId::OpencodeGo, "scope").unwrap();
        for key in ["reasoning_effort", "reasoning", "output_config"] {
            assert!(encoded.get(key).is_none(), "default must omit {key}");
        }
        input.settings.reasoning_effort = Some("unsupported".into());
        assert!(input.validate(provider, &caps).is_err());
    }
}

#[test]
fn all_wire_shapes_preserve_calls_results_private_continuation_and_usage() {
    for wire in [
        WireProtocol::OpenAiChatCompletions,
        WireProtocol::OpenAiResponses,
        WireProtocol::AnthropicMessages,
    ] {
        let mut input = request(wire);
        let response = decode(&terminal(wire), wire, &input, ProviderId::OpencodeGo).unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.usage.total_tokens, Some(10));
        let continuation = response.message.continuation.as_ref().unwrap();
        assert!(continuation.required);
        assert!(!format!("{continuation:?}").contains("private"));
        let (call_id, provider_call_id) = response
            .message
            .blocks
            .iter()
            .find_map(|b| {
                if let BlockContent::ToolCall {
                    call_id,
                    provider_call_id,
                    ..
                } = &b.content
                {
                    Some((call_id.clone(), provider_call_id.clone()))
                } else {
                    None
                }
            })
            .unwrap();
        input.messages.push(response.message);
        input.messages.push(InferenceMessage {
            role: MessageRole::Tool,
            blocks: vec![ContentBlock {
                id: "result".into(),
                content: BlockContent::ToolResult {
                    call_id,
                    provider_call_id,
                    is_error: true,
                    output: "Interrupted".into(),
                    artifact_ids: vec!["artifact-1".into()],
                    effects_unknown: true,
                },
            }],
            continuation: None,
        });
        input
            .validate(
                OpencodeGoProvider::instance(),
                &inference::capabilities(OpencodeGoProvider::instance(), &input.model),
            )
            .unwrap();
        let body = encode(&input, wire, ProviderId::OpencodeGo, "").unwrap();
        let encoded = body.to_string();
        assert!(encoded.contains("provider-call"));
        assert!(encoded.contains("effects_unknown"));
        assert!(encoded.contains("artifact-1"));
        assert!(encoded.contains("private"));
        input.model = "glm-5.3".into();
        assert!(matches!(
            input.validate(
                OpencodeGoProvider::instance(),
                &inference::capabilities(OpencodeGoProvider::instance(), &input.model)
            ),
            Err(crate::providers::ProviderError::UnsupportedCapability { .. })
        ));
    }
}

#[test]
fn streamed_call_arguments_survive_every_byte_boundary_and_require_terminal_evidence() {
    let input = request(WireProtocol::OpenAiChatCompletions);
    let values = [
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"provider-call","type":"function","function":{"name":"read","arguments":"{\"pa"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"☃\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}),
    ];
    let body = values
        .iter()
        .map(|value| format!("data: {value}\r\n\r\n"))
        .collect::<String>()
        + "data: [DONE]\r\n\r\n";
    for split in 0..=body.len() {
        let mut framing = SseStream::new();
        let mut fold = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
        let mut fragments = String::new();
        let mut callback = |event| {
            if let ProviderEvent::ToolArgumentsDelta { fragment, .. } = event {
                fragments.push_str(&fragment)
            }
        };
        for chunk in [&body.as_bytes()[..split], &body.as_bytes()[split..]] {
            for event in framing.push(chunk).unwrap() {
                fold.feed(&event, &mut callback).unwrap();
            }
        }
        for event in framing.finish().unwrap() {
            fold.feed(&event, &mut callback).unwrap();
        }
        let response = fold.finish(&input, ProviderId::OpencodeGo).unwrap();
        assert_eq!(response.usage.total_tokens, Some(10));
        assert_eq!(fragments, "{\"path\":\"☃\"}");
        assert!(
            matches!(&response.message.blocks[1].content,BlockContent::ToolCall{arguments,..} if arguments==&json!({"path":"☃"}))
        );
    }
    let mut truncated = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
    truncated
        .feed(&format!("data: {}\n\n", values[0]), &mut |_| {})
        .unwrap();
    assert!(truncated.finish(&input, ProviderId::OpencodeGo).is_err());
}

#[test]
fn incomplete_or_malformed_proposals_never_become_calls() {
    let input = request(WireProtocol::OpenAiChatCompletions);
    let mut value = terminal(WireProtocol::OpenAiChatCompletions);
    value["choices"][0]["finish_reason"] = "length".into();
    value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = "{broken".into();
    let response = decode(
        &value,
        WireProtocol::OpenAiChatCompletions,
        &input,
        ProviderId::OpencodeGo,
    )
    .unwrap();
    assert_eq!(response.finish_reason, FinishReason::OutputLimit);
    assert!(response.message.continuation.is_none());
    assert!(
        !response
            .message
            .blocks
            .iter()
            .any(|b| matches!(b.content, BlockContent::ToolCall { .. }))
    );
    value["choices"][0]["finish_reason"] = "tool_calls".into();
    assert!(
        decode(
            &value,
            WireProtocol::OpenAiChatCompletions,
            &input,
            ProviderId::OpencodeGo
        )
        .is_err()
    );
    value["choices"][0]["finish_reason"] = "stop".into();
    assert!(
        decode(
            &value,
            WireProtocol::OpenAiChatCompletions,
            &input,
            ProviderId::OpencodeGo
        )
        .is_err()
    );
    let mut fold = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
    fold.feed(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        &mut |_| {},
    )
    .unwrap();
    assert!(
        fold.feed(
            "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n",
            &mut |_| {}
        )
        .is_err()
    );
    let mut input = request(WireProtocol::OpenAiResponses);
    input.settings.reasoning_effort = Some("ultra".into());
    assert!(
        input
            .validate(
                OpencodeGoProvider::instance(),
                &inference::capabilities(OpencodeGoProvider::instance(), &input.model)
            )
            .is_err()
    );
}

#[test]
fn incomplete_messages_discard_partial_json_and_end_markers_preserve_outcome() {
    let input = request(WireProtocol::AnthropicMessages);
    let mut fold = StructuredFold::new(WireProtocol::AnthropicMessages);
    for value in [
        json!({"type":"message_start","message":{"content":[],"model":"minimax-m3"}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"provider-call","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
        json!({"type":"message_stop"}),
    ] {
        fold.feed(&format!("data: {value}\n\n"), &mut |_| {})
            .unwrap();
    }
    fold.feed("data: [DONE]\n\n", &mut |_| {}).unwrap();
    let result = fold.finish(&input, ProviderId::OpencodeGo).unwrap();
    assert_eq!(result.finish_reason, FinishReason::OutputLimit);
    assert!(result.message.continuation.is_none());
    assert!(
        !result
            .message
            .blocks
            .iter()
            .any(|b| matches!(b.content, BlockContent::ToolCall { .. }))
    );
}

#[test]
fn tool_indexes_and_result_pairing_are_checked_and_refusals_are_explicit() {
    let mut input = request(WireProtocol::OpenAiChatCompletions);
    let mut fold = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
    fold.feed("data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":2,\"id\":\"id\",\"type\":\"function\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",&mut |_| {}).unwrap();
    assert!(fold.finish(&input, ProviderId::OpencodeGo).is_err());
    let refusal =
        json!({"choices":[{"message":{"refusal":"Cannot comply"},"finish_reason":"stop"}]});
    let result = decode(
        &refusal,
        WireProtocol::OpenAiChatCompletions,
        &input,
        ProviderId::OpencodeGo,
    )
    .unwrap();
    assert_eq!(result.finish_reason, FinishReason::Refusal);
    let response = decode(
        &terminal(WireProtocol::OpenAiChatCompletions),
        WireProtocol::OpenAiChatCompletions,
        &input,
        ProviderId::OpencodeGo,
    )
    .unwrap();
    let (call_id, provider_call_id) = response
        .message
        .blocks
        .iter()
        .find_map(|b| match &b.content {
            BlockContent::ToolCall {
                call_id,
                provider_call_id,
                ..
            } => Some((call_id.clone(), provider_call_id.clone())),
            _ => None,
        })
        .unwrap();
    input.messages.push(response.message);
    input.messages.push(InferenceMessage {
        role: MessageRole::Tool,
        continuation: None,
        blocks: vec![ContentBlock {
            id: "result".into(),
            content: BlockContent::ToolResult {
                call_id,
                provider_call_id: format!("wrong-{provider_call_id}"),
                is_error: false,
                output: "ok".into(),
                artifact_ids: vec![],
                effects_unknown: false,
            },
        }],
    });
    assert!(
        input
            .validate(
                OpencodeGoProvider::instance(),
                &inference::capabilities(OpencodeGoProvider::instance(), &input.model)
            )
            .is_err()
    );
}

#[test]
fn responses_and_messages_streams_assemble_completed_calls() {
    let responses = request(WireProtocol::OpenAiResponses);
    let mut fold = StructuredFold::new(WireProtocol::OpenAiResponses);
    for value in [
        json!({"type":"response.output_text.delta","output_index":1,"content_index":0,"delta":"Inspecting."}),
        json!({"type":"response.function_call_arguments.delta","output_index":2,"delta":"{\"path\":\"Cargo.toml\"}"}),
        json!({"type":"response.completed","response":terminal(WireProtocol::OpenAiResponses)}),
    ] {
        fold.feed(&format!("data: {value}\n\n"), &mut |_| {})
            .unwrap();
    }
    assert_eq!(
        fold.finish(&responses, ProviderId::OpencodeGo)
            .unwrap()
            .finish_reason,
        FinishReason::ToolCalls
    );
    let messages = request(WireProtocol::AnthropicMessages);
    let mut fold = StructuredFold::new(WireProtocol::AnthropicMessages);
    for value in [
        json!({"type":"message_start","message":{"model":"minimax-m3","content":[],"usage":{"input_tokens":7}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"provider-call","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"Cargo.toml\"}"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ] {
        fold.feed(&format!("data: {value}\n\n"), &mut |_| {})
            .unwrap();
    }
    let response = fold.finish(&messages, ProviderId::OpencodeGo).unwrap();
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.usage.total_tokens, Some(10));
}

#[test]
fn messages_streams_append_long_text_and_preserve_signed_reasoning() {
    let input = request(WireProtocol::AnthropicMessages);
    let mut fold = StructuredFold::new(WireProtocol::AnthropicMessages);
    let mut visible = String::new();
    let mut callback = |event| {
        if let ProviderEvent::TextDelta { text, .. } = event {
            visible.push_str(&text);
        }
    };
    for value in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"private ","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"reasoning"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"prefix "}}),
    ] {
        fold.feed(&format!("data: {value}\n\n"), &mut callback)
            .unwrap();
    }
    let fragment = "☃\n".repeat(256);
    for _ in 0..300 {
        let value = json!({"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":fragment}});
        fold.feed(&format!("data: {value}\n\n"), &mut callback)
            .unwrap();
    }
    for value in [
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"provider-call","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":\"Cargo.toml\"}"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
        json!({"type":"message_stop"}),
    ] {
        fold.feed(&format!("data: {value}\n\n"), &mut callback)
            .unwrap();
    }
    let response = fold.finish(&input, ProviderId::OpencodeGo).unwrap();
    assert_eq!(visible, format!("prefix {}", fragment.repeat(300)));
    assert_eq!(response.message.visible_text(), visible);
    let continuation = response.message.continuation.unwrap();
    assert!(continuation.required);
    assert_eq!(continuation.items[0]["thinking"], "private reasoning");
    assert_eq!(continuation.items[0]["signature"], "signed");
}

#[test]
fn streamed_visible_limit_applies_across_content_blocks() {
    use crate::providers::opencode::{DecodeError, MAX_STREAM_TEXT_BYTES};
    for wire in [
        WireProtocol::AnthropicMessages,
        WireProtocol::OpenAiResponses,
    ] {
        let mut fold = StructuredFold::new(wire);
        let text = "x".repeat(MAX_STREAM_TEXT_BYTES / 2 + 1);
        let event = |index| match wire {
            WireProtocol::AnthropicMessages => {
                json!({"type":"content_block_start","index":index,"content_block":{"type":"text","text":text}})
            }
            _ => {
                json!({"type":"response.output_text.delta","output_index":index,"content_index":0,"delta":text})
            }
        };
        fold.feed(&format!("data: {}\n\n", event(0)), &mut |_| {})
            .unwrap();
        assert!(matches!(
            fold.feed(&format!("data: {}\n\n", event(1)), &mut |_| {}),
            Err(DecodeError::LimitExceeded { .. })
        ));
    }
}

#[test]
fn repeated_provider_call_ids_keep_distinct_results_with_and_without_private_replay() {
    for wire in [
        WireProtocol::OpenAiChatCompletions,
        WireProtocol::OpenAiResponses,
        WireProtocol::AnthropicMessages,
    ] {
        for private_replay in [false, true] {
            let mut input = request(wire);
            for index in 0..2 {
                input.request_id = format!("request-{index}");
                let mut message = decode(&terminal(wire), wire, &input, ProviderId::OpencodeGo)
                    .unwrap()
                    .message;
                if !private_replay {
                    message.continuation = None;
                }
                let (call_id, provider_call_id) = message
                    .blocks
                    .iter()
                    .find_map(|block| match &block.content {
                        BlockContent::ToolCall {
                            call_id,
                            provider_call_id,
                            ..
                        } => Some((call_id.clone(), provider_call_id.clone())),
                        _ => None,
                    })
                    .unwrap();
                input.messages.push(message);
                input.messages.push(InferenceMessage {
                    role: MessageRole::Tool,
                    continuation: None,
                    blocks: vec![ContentBlock {
                        id: format!("result-{index}"),
                        content: BlockContent::ToolResult {
                            call_id,
                            provider_call_id,
                            output: format!("result-{index}"),
                            is_error: false,
                            artifact_ids: vec![],
                            effects_unknown: false,
                        },
                    }],
                });
            }
            input
                .validate(
                    OpencodeGoProvider::instance(),
                    &inference::capabilities(OpencodeGoProvider::instance(), &input.model),
                )
                .unwrap();
            let stored = input.messages.clone();
            let body = encode(&input, wire, ProviderId::OpencodeGo, "").unwrap();
            assert_eq!(input.messages, stored);
            assert_eq!(
                body,
                encode(&input, wire, ProviderId::OpencodeGo, "").unwrap()
            );
            let (calls, results) = encoded_call_ids(&body, wire);
            assert_eq!(calls.len(), 2);
            assert_eq!(results, calls);
            assert_eq!(calls[0], "provider-call");
            assert_ne!(calls[0], calls[1]);
            assert!(calls[1].len() <= 64);
            assert_eq!(body.to_string().contains("private"), private_replay);
        }
    }
}

fn encoded_call_ids(body: &Value, wire: WireProtocol) -> (Vec<String>, Vec<String>) {
    let mut calls = Vec::new();
    let mut results = Vec::new();
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            for message in body["messages"].as_array().unwrap() {
                if let Some(items) = message["tool_calls"].as_array() {
                    calls.extend(
                        items
                            .iter()
                            .map(|item| item["id"].as_str().unwrap().to_owned()),
                    );
                }
                if message["role"] == "tool" {
                    results.push(message["tool_call_id"].as_str().unwrap().to_owned());
                }
            }
        }
        WireProtocol::OpenAiResponses => {
            for item in body["input"].as_array().unwrap() {
                if item["type"] == "function_call" {
                    calls.push(item["call_id"].as_str().unwrap().to_owned());
                } else if item["type"] == "function_call_output" {
                    results.push(item["call_id"].as_str().unwrap().to_owned());
                }
            }
        }
        WireProtocol::AnthropicMessages => {
            for message in body["messages"].as_array().unwrap() {
                for item in message["content"].as_array().unwrap() {
                    if item["type"] == "tool_use" {
                        calls.push(item["id"].as_str().unwrap().to_owned());
                    } else if item["type"] == "tool_result" {
                        results.push(item["tool_use_id"].as_str().unwrap().to_owned());
                    }
                }
            }
        }
    }
    (calls, results)
}

#[test]
fn streamed_chat_refusals_are_visible_and_replayed_as_refusals() {
    let mut input = request(WireProtocol::OpenAiChatCompletions);
    let mut fold = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
    let mut deltas = Vec::new();
    for delta in [
        json!({"content":"I checked. "}),
        json!({"refusal":"Cannot "}),
        json!({"refusal":"comply ☃"}),
    ] {
        fold.feed(
            &format!("data: {}\n\n", json!({"choices":[{"delta":delta}]})),
            &mut |event| deltas.push(event),
        )
        .unwrap();
    }
    fold.feed(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        &mut |_| {},
    )
    .unwrap();
    let response = fold.finish(&input, ProviderId::OpencodeGo).unwrap();
    assert_eq!(response.finish_reason, FinishReason::Refusal);
    let visible: String = deltas
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(visible, response.message.visible_text());
    assert_eq!(visible, "I checked. Cannot comply ☃");
    assert!(
        matches!(deltas[1], ProviderEvent::TextDelta { block_index, .. } if block_index == MAX_TOOL_CALLS + 1)
    );
    assert!(
        matches!(deltas[2], ProviderEvent::TextDelta { block_index, .. } if block_index == MAX_TOOL_CALLS + 1)
    );
    input.messages.push(response.message);
    let body = encode(
        &input,
        WireProtocol::OpenAiChatCompletions,
        ProviderId::OpencodeGo,
        "",
    )
    .unwrap();
    assert_eq!(body["messages"][1]["content"], "I checked. ");
    assert_eq!(body["messages"][1]["refusal"], "Cannot comply ☃");
}

#[test]
fn streamed_chat_refusals_share_the_visible_output_byte_limit() {
    use crate::providers::opencode::{DecodeError, MAX_STREAM_TEXT_BYTES};

    let mut fold = StructuredFold::new(WireProtocol::OpenAiChatCompletions);
    fold.feed(
        &format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":"x".repeat(MAX_STREAM_TEXT_BYTES / 2)}}]})
        ),
        &mut |_| {},
    )
    .unwrap();
    let mut emitted = false;
    let result = fold.feed(
        &format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"refusal":"x".repeat(MAX_STREAM_TEXT_BYTES / 2 + 1)}}]})
        ),
        &mut |_| emitted = true,
    );
    assert!(matches!(result, Err(DecodeError::LimitExceeded { .. })));
    assert!(!emitted);
}
