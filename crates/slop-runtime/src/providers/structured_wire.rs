//! Bounded translation for structured gateway requests. Transport framing and
//! service identity remain outside these decoders.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use slop_core::provider::ProviderId;

use super::{Usage, WireProtocol, inference::*, opencode::DecodeError};

type Result<T> = std::result::Result<T, DecodeError>;

fn invalid(detail: &'static str) -> DecodeError {
    DecodeError::InvalidResponse { detail }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::{opencode::SseStream, opencode_go::OpencodeGoProvider};

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
                    &capabilities(OpencodeGoProvider::instance(), &input.model),
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
                    &capabilities(OpencodeGoProvider::instance(), &input.model)
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
                    &capabilities(OpencodeGoProvider::instance(), &input.model)
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
                    &capabilities(OpencodeGoProvider::instance(), &input.model)
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
}
fn unsupported() -> DecodeError {
    DecodeError::UnsupportedCapability {
        capability: "unsupported structured content",
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing structured string"))
}
fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("missing structured array"))
}
fn append(target: &mut String, text: &str, limit: usize) -> Result<()> {
    if target.len().saturating_add(text.len()) > limit {
        return Err(DecodeError::LimitExceeded {
            detail: "structured stream exceeds byte bound",
        });
    }
    target.push_str(text);
    Ok(())
}
fn usage(value: &Value) -> Usage {
    let u = value.get("usage").unwrap_or(&Value::Null);
    Usage::from_reported(
        u.get("input_tokens")
            .or_else(|| u.get("prompt_tokens"))
            .and_then(Value::as_u64),
        u.get("output_tokens")
            .or_else(|| u.get("completion_tokens"))
            .and_then(Value::as_u64),
        u.get("total_tokens").and_then(Value::as_u64),
    )
}
fn role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn tool_output(content: &BlockContent) -> String {
    match content {
        BlockContent::ToolResult{output,is_error,artifact_ids,effects_unknown,..}=>json!({"output":output,"is_error":is_error,"artifact_ids":artifact_ids,"effects_unknown":effects_unknown}).to_string(),
        _=>String::new(),
    }
}

pub(super) fn encode(
    request: &InferenceRequest,
    wire: WireProtocol,
    provider: ProviderId,
    scope: &str,
) -> std::result::Result<Value, super::ProviderError> {
    let mut body = json!({"model":request.model,"stream":true});
    let mut messages = Vec::new();
    let mut system = None;
    for message in &request.messages {
        let original = message.continuation.as_ref().filter(|c| {
            c.model == request.model
                && c.wire == wire.as_str()
                && c.provider == provider.as_str()
                && c.scope == scope
        });
        match wire {
            WireProtocol::OpenAiChatCompletions => {
                if let Some(original) = original {
                    messages.extend(original.items.clone());
                    continue;
                }
                if message.role == MessageRole::Tool {
                    for block in &message.blocks {
                        if let BlockContent::ToolResult {
                            provider_call_id, ..
                        } = &block.content
                        {
                            messages.push(json!({"role":"tool","tool_call_id":provider_call_id,"content":tool_output(&block.content)}));
                        }
                    }
                    continue;
                }
                let mut value =
                    json!({"role":role(&message.role),"content":message.visible_text()});
                let calls: Vec<_> = message.blocks.iter().filter_map(|b| match &b.content {
                    BlockContent::ToolCall { provider_call_id, name, arguments, .. } => Some(json!({"id":provider_call_id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}})),
                    _ => None,
                }).collect();
                if !calls.is_empty() {
                    value["tool_calls"] = calls.into();
                }
                messages.push(value);
            }
            WireProtocol::OpenAiResponses => {
                if let Some(original) = original {
                    messages.extend(original.items.clone());
                    continue;
                }
                for block in &message.blocks {
                    messages.push(match &block.content {
                        BlockContent::Text { text } | BlockContent::Refusal { text } => json!({"role":role(&message.role),"content":text}),
                        BlockContent::ToolCall { provider_call_id, name, arguments, .. } => json!({"type":"function_call","call_id":provider_call_id,"name":name,"arguments":arguments.to_string()}),
                        BlockContent::ToolResult { provider_call_id, .. } => json!({"type":"function_call_output","call_id":provider_call_id,"output":tool_output(&block.content)}),
                    });
                }
            }
            WireProtocol::AnthropicMessages => {
                if message.role == MessageRole::System {
                    system = Some(message.visible_text());
                    continue;
                }
                let content = if let Some(original) = original {
                    original.items.clone()
                } else {
                    message.blocks.iter().map(|block| match &block.content {
                        BlockContent::Text { text } | BlockContent::Refusal { text } => json!({"type":"text","text":text}),
                        BlockContent::ToolCall { provider_call_id, name, arguments, .. } => json!({"type":"tool_use","id":provider_call_id,"name":name,"input":arguments}),
                        BlockContent::ToolResult { provider_call_id, is_error, .. } => json!({"type":"tool_result","tool_use_id":provider_call_id,"content":tool_output(&block.content),"is_error":is_error}),
                    }).collect()
                };
                let author = if message.role == MessageRole::Tool {
                    "user"
                } else {
                    role(&message.role)
                };
                // Results of a parallel proposal must form one user message.
                if author == "user"
                    && let Some(last) = messages.last_mut()
                    && last.get("role").and_then(Value::as_str) == Some("user")
                {
                    last["content"]
                        .as_array_mut()
                        .expect("constructed content array")
                        .extend(content);
                } else {
                    messages.push(json!({"role":author,"content":content}));
                }
            }
        }
    }
    let tools: Vec<_> = request.tools.iter().map(|tool| match wire {
        WireProtocol::OpenAiChatCompletions => json!({"type":"function","function":{"name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false}}),
        WireProtocol::OpenAiResponses => json!({"type":"function","name":tool.name,"description":tool.description,"parameters":tool.parameters,"strict":false}),
        WireProtocol::AnthropicMessages => json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters}),
    }).collect();
    if !tools.is_empty() {
        body["tools"] = tools.into();
    }
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            body["messages"] = messages.into();
            body["max_tokens"] = request.settings.max_output_tokens.into();
            body["stream_options"] = json!({"include_usage":true});
        }
        WireProtocol::OpenAiResponses => {
            body["input"] = messages.into();
            body["max_output_tokens"] = request.settings.max_output_tokens.into();
            body["store"] = false.into();
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        WireProtocol::AnthropicMessages => {
            body["messages"] = messages.into();
            body["max_tokens"] = request.settings.max_output_tokens.into();
            if let Some(system) = system {
                body["system"] = system.into();
            }
        }
    }
    // Effort cannot reach here unless a model descriptor advertises support.
    if let Some(effort) = &request.settings.reasoning_effort {
        match wire {
            WireProtocol::OpenAiResponses => body["reasoning"] = json!({"effort":effort}),
            WireProtocol::OpenAiChatCompletions => body["reasoning_effort"] = effort.clone().into(),
            WireProtocol::AnthropicMessages => {
                return Err(super::ProviderError::UnsupportedCapability {
                    capability: "settings.reasoning_effort",
                });
            }
        }
    }
    Ok(body)
}

fn block(request: &InferenceRequest, index: usize, content: BlockContent) -> ContentBlock {
    ContentBlock {
        id: format!("{}:block:{index}", request.request_id),
        content,
    }
}
fn call(
    request: &InferenceRequest,
    index: usize,
    provider_call_id: &str,
    name: &str,
    arguments: Value,
) -> Result<ContentBlock> {
    if provider_call_id.is_empty()
        || provider_call_id.len() > 256
        || !valid_name(name)
        || !arguments.is_object()
        || arguments.to_string().len() > MAX_ARGUMENT_BYTES
    {
        return Err(invalid("invalid completed tool proposal"));
    }
    Ok(block(
        request,
        index,
        BlockContent::ToolCall {
            call_id: format!("{}:call:{index}", request.request_id),
            provider_call_id: provider_call_id.to_owned(),
            name: name.to_owned(),
            arguments,
        },
    ))
}

pub(super) fn decode(
    value: &Value,
    wire: WireProtocol,
    request: &InferenceRequest,
    provider: ProviderId,
) -> Result<InferenceResponse> {
    if value.get("error").is_some_and(|e| !e.is_null()) {
        return Err(DecodeError::TurnFailed);
    }
    let mut blocks = Vec::new();
    let mut private_items = Vec::new();
    let mut finish_reason = match wire {
        WireProtocol::OpenAiChatCompletions => {
            let choices = array(value, "choices")?;
            if choices.len() != 1 {
                return Err(unsupported());
            }
            let message = choices[0]
                .get("message")
                .ok_or_else(|| invalid("missing assistant message"))?;
            if message.get("audio").is_some_and(|v| !v.is_null())
                || message.get("function_call").is_some_and(|v| !v.is_null())
            {
                return Err(unsupported());
            }
            let finish = match string(&choices[0], "finish_reason")? {
                "stop" => FinishReason::Stop,
                "tool_calls" => FinishReason::ToolCalls,
                "length" | "content_filter" => FinishReason::OutputLimit,
                _ => return Err(invalid("unknown chat finish reason")),
            };
            if let Some(text) = message.get("content").filter(|v| !v.is_null()) {
                blocks.push(block(
                    request,
                    0,
                    BlockContent::Text {
                        text: text.as_str().ok_or_else(unsupported)?.to_owned(),
                    },
                ));
            }
            if let Some(refusal) = message.get("refusal").filter(|v| !v.is_null()) {
                blocks.push(block(
                    request,
                    MAX_TOOL_CALLS + 1,
                    BlockContent::Refusal {
                        text: refusal.as_str().ok_or_else(unsupported)?.to_owned(),
                    },
                ));
            }
            if let Some(calls) = message.get("tool_calls").filter(|v| !v.is_null()) {
                let calls = calls
                    .as_array()
                    .ok_or_else(|| invalid("invalid tool-call array"))?;
                if calls.len() > MAX_TOOL_CALLS {
                    return Err(invalid("too many tool proposals"));
                }
                if !calls.is_empty() && finish == FinishReason::Stop {
                    return Err(invalid("tool calls contradict finish reason"));
                }
                if finish == FinishReason::ToolCalls {
                    for (index, c) in calls.iter().enumerate() {
                        if string(c, "type")? != "function" {
                            return Err(unsupported());
                        }
                        let f = c
                            .get("function")
                            .ok_or_else(|| invalid("missing function"))?;
                        let arguments: Value = serde_json::from_str(string(f, "arguments")?)
                            .map_err(|_| invalid("invalid completed tool arguments"))?;
                        blocks.push(call(
                            request,
                            index + 1,
                            string(c, "id")?,
                            string(f, "name")?,
                            arguments,
                        )?);
                    }
                }
            }
            if let Some(reasoning) = message.get("reasoning_content").filter(|v| !v.is_null()) {
                if !reasoning.is_string() {
                    return Err(unsupported());
                }
                if !reasoning.as_str().unwrap_or_default().is_empty() {
                    private_items.push(message.clone());
                }
            }
            finish
        }
        WireProtocol::OpenAiResponses => {
            let status = string(value, "status")?;
            if status == "failed" {
                return Err(DecodeError::TurnFailed);
            }
            if !matches!(status, "completed" | "incomplete") {
                return Err(invalid("missing responses terminal status"));
            }
            let output = array(value, "output")?;
            if output.len() > MAX_BLOCKS {
                return Err(invalid("too many response items"));
            }
            let mut private = false;
            for (index, item) in output.iter().enumerate() {
                match string(item, "type")? {
                    "message" => {
                        for (part_index, part) in array(item, "content")?.iter().enumerate() {
                            if part_index >= MAX_BLOCKS {
                                return Err(invalid("too many content parts"));
                            }
                            let content = match string(part, "type")? {
                                "output_text" => BlockContent::Text {
                                    text: string(part, "text")?.to_owned(),
                                },
                                "refusal" => BlockContent::Refusal {
                                    text: string(part, "refusal")?.to_owned(),
                                },
                                _ => return Err(unsupported()),
                            };
                            blocks.push(block(request, index * MAX_BLOCKS + part_index, content));
                        }
                    }
                    "function_call" if status == "completed" => {
                        if item
                            .get("status")
                            .and_then(Value::as_str)
                            .is_some_and(|s| s != "completed")
                        {
                            return Err(invalid("incomplete function-call item"));
                        }
                        let args = serde_json::from_str(string(item, "arguments")?)
                            .map_err(|_| invalid("invalid completed tool arguments"))?;
                        blocks.push(call(
                            request,
                            index * MAX_BLOCKS,
                            string(item, "call_id")?,
                            string(item, "name")?,
                            args,
                        )?);
                    }
                    "function_call" => {}
                    "reasoning" => {
                        private = true;
                    }
                    _ => return Err(unsupported()),
                }
            }
            if private {
                private_items = output.to_vec();
            }
            if status == "incomplete" {
                FinishReason::OutputLimit
            } else if blocks
                .iter()
                .any(|b| matches!(b.content, BlockContent::ToolCall { .. }))
            {
                FinishReason::ToolCalls
            } else {
                FinishReason::Stop
            }
        }
        WireProtocol::AnthropicMessages => {
            let finish = match string(value, "stop_reason")? {
                "end_turn" | "stop_sequence" => FinishReason::Stop,
                "tool_use" => FinishReason::ToolCalls,
                "max_tokens" => FinishReason::OutputLimit,
                "refusal" => FinishReason::Refusal,
                _ => return Err(invalid("unknown messages finish reason")),
            };
            let content = array(value, "content")?;
            let mut private = false;
            for (index, item) in content.iter().enumerate() {
                match string(item, "type")? {
                    "text" => blocks.push(block(
                        request,
                        index,
                        BlockContent::Text {
                            text: string(item, "text")?.to_owned(),
                        },
                    )),
                    "tool_use" if finish == FinishReason::ToolCalls => blocks.push(call(
                        request,
                        index,
                        string(item, "id")?,
                        string(item, "name")?,
                        item.get("input")
                            .cloned()
                            .ok_or_else(|| invalid("missing tool input"))?,
                    )?),
                    "tool_use" if finish == FinishReason::OutputLimit => {}
                    "tool_use" => return Err(invalid("tool calls contradict finish reason")),
                    "thinking" | "redacted_thinking" => {
                        private = true;
                    }
                    _ => return Err(unsupported()),
                }
            }
            if private {
                private_items = content.to_vec();
            }
            finish
        }
    };
    let calls: Vec<_> = blocks
        .iter()
        .filter_map(|b| match &b.content {
            BlockContent::ToolCall {
                provider_call_id, ..
            } => Some(provider_call_id),
            _ => None,
        })
        .collect();
    if calls.len() > MAX_TOOL_CALLS
        || calls.iter().collect::<std::collections::HashSet<_>>().len() != calls.len()
    {
        return Err(invalid("duplicate or excessive tool calls"));
    }
    if blocks
        .iter()
        .any(|b| matches!(b.content, BlockContent::Refusal { .. }))
    {
        if !calls.is_empty() {
            return Err(invalid("refusal contradicts tool proposals"));
        }
        if finish_reason == FinishReason::Stop {
            finish_reason = FinishReason::Refusal;
        }
    }
    if finish_reason == FinishReason::ToolCalls && calls.is_empty() {
        return Err(invalid("tool finish reason without a proposal"));
    }
    if blocks.len() > MAX_BLOCKS
        || serde_json::to_vec(&blocks)
            .map_err(|_| invalid("invalid blocks"))?
            .len()
            > super::opencode::MAX_BODY_BYTES
    {
        return Err(invalid("structured output exceeds bounds"));
    }
    let text_bytes: usize = blocks
        .iter()
        .filter_map(|b| match &b.content {
            BlockContent::Text { text } | BlockContent::Refusal { text } => Some(text.len()),
            _ => None,
        })
        .sum();
    if text_bytes > super::opencode::MAX_STREAM_TEXT_BYTES {
        return Err(invalid("visible output exceeds bound"));
    }
    let continuation = if private_items.is_empty() || finish_reason == FinishReason::OutputLimit {
        None
    } else {
        Some(Continuation {
            scope: String::new(),
            provider: provider.as_str().to_owned(),
            model: request.model.clone(),
            wire: wire.as_str().to_owned(),
            required: finish_reason == FinishReason::ToolCalls,
            items: private_items,
        })
    };
    if blocks.is_empty() {
        blocks.push(block(
            request,
            0,
            BlockContent::Text {
                text: String::new(),
            },
        ));
    }
    Ok(InferenceResponse {
        resolved_model: super::opencode::reported_model(value)?,
        message: InferenceMessage {
            role: MessageRole::Assistant,
            blocks,
            continuation,
        },
        usage: usage(value),
        finish_reason,
    })
}

#[derive(Default)]
struct CallParts {
    id: String,
    name: String,
    arguments: String,
}

pub(super) struct StructuredFold {
    wire: WireProtocol,
    text: String,
    reasoning: String,
    refusal: String,
    calls: BTreeMap<usize, CallParts>,
    anthropic: BTreeMap<usize, Value>,
    argument_parts: BTreeMap<usize, String>,
    closed: std::collections::HashSet<usize>,
    terminal: Option<Value>,
    stop_reason: Option<String>,
    model: Option<String>,
    usage: Usage,
}

impl StructuredFold {
    pub(super) fn new(wire: WireProtocol) -> Self {
        Self {
            wire,
            text: String::new(),
            reasoning: String::new(),
            refusal: String::new(),
            calls: BTreeMap::new(),
            anthropic: BTreeMap::new(),
            argument_parts: BTreeMap::new(),
            closed: std::collections::HashSet::new(),
            terminal: None,
            stop_reason: None,
            model: None,
            usage: Usage::default(),
        }
    }

    pub(super) fn feed(
        &mut self,
        event: &str,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        let payload = event
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|l| l.strip_prefix(' ').unwrap_or(l))
            .collect::<Vec<_>>()
            .join("\n");
        if payload.trim() == "[DONE]" {
            return if self.terminal.is_some() {
                Ok(())
            } else {
                Err(invalid("end marker before terminal evidence"))
            };
        }
        let value: Value =
            serde_json::from_str(&payload).map_err(|_| invalid("invalid stream JSON"))?;
        if value.get("error").is_some_and(|e| !e.is_null())
            || matches!(
                value.get("type").and_then(Value::as_str),
                Some("error" | "response.failed" | "response.error")
            )
        {
            return Err(DecodeError::TurnFailed);
        }
        if let Some(model) = super::opencode::reported_model(&value)? {
            self.model = Some(model);
        }
        self.usage.merge(usage(&value));
        match self.wire {
            WireProtocol::OpenAiChatCompletions => self.chat(&value, on_event)?,
            WireProtocol::OpenAiResponses => self.responses(&value, on_event)?,
            WireProtocol::AnthropicMessages => self.messages(&value, on_event)?,
        }
        if value.get("usage").is_some() {
            on_event(ProviderEvent::UsageUpdated { usage: self.usage });
        }
        Ok(())
    }

    fn text_delta(
        &mut self,
        index: usize,
        text: &str,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        if self.terminal.is_some() {
            return Err(invalid("content after terminal outcome"));
        }
        append(&mut self.text, text, super::opencode::MAX_STREAM_TEXT_BYTES)?;
        if !text.is_empty() {
            on_event(ProviderEvent::TextDelta {
                block_index: index,
                text: text.to_owned(),
            });
        }
        Ok(())
    }

    fn arguments_delta(
        &mut self,
        index: usize,
        text: &str,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        if self.terminal.is_some() || index >= MAX_BLOCKS * MAX_BLOCKS {
            return Err(invalid("invalid tool delta"));
        }
        on_event(ProviderEvent::ToolArgumentsDelta {
            block_index: index,
            fragment: text.to_owned(),
        });
        Ok(())
    }

    fn chat(
        &mut self,
        value: &Value,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        let choices = array(value, "choices")?;
        if choices.is_empty() {
            return if value.get("usage").is_some() {
                Ok(())
            } else {
                Err(invalid("empty chat stream choices"))
            };
        }
        if choices.len() != 1 {
            return Err(unsupported());
        }
        let choice = &choices[0];
        if let Some(delta) = choice.get("delta") {
            if delta.get("audio").is_some_and(|v| !v.is_null())
                || delta.get("function_call").is_some_and(|v| !v.is_null())
            {
                return Err(unsupported());
            }
            if let Some(text) = delta.get("content").filter(|v| !v.is_null()) {
                self.text_delta(0, text.as_str().ok_or_else(unsupported)?, on_event)?;
            }
            if let Some(text) = delta.get("refusal").filter(|v| !v.is_null()) {
                if self.terminal.is_some() {
                    return Err(invalid("refusal after terminal"));
                }
                append(
                    &mut self.refusal,
                    text.as_str().ok_or_else(unsupported)?,
                    super::opencode::MAX_STREAM_TEXT_BYTES,
                )?;
            }
            if let Some(text) = delta.get("reasoning_content").filter(|v| !v.is_null()) {
                if self.terminal.is_some() {
                    return Err(invalid("reasoning after terminal"));
                }
                append(
                    &mut self.reasoning,
                    text.as_str().ok_or_else(unsupported)?,
                    super::opencode::MAX_STREAM_TEXT_BYTES,
                )?;
            }
            if let Some(calls) = delta.get("tool_calls").filter(|v| !v.is_null()) {
                if self.terminal.is_some() {
                    return Err(invalid("tools after terminal"));
                }
                for c in calls
                    .as_array()
                    .ok_or_else(|| invalid("invalid streamed calls"))?
                {
                    let index = c
                        .get("index")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| invalid("missing tool index"))?
                        as usize;
                    if index >= MAX_TOOL_CALLS {
                        return Err(invalid("excessive tool index"));
                    }
                    if c.get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t != "function")
                    {
                        return Err(unsupported());
                    }
                    let parts = self.calls.entry(index).or_default();
                    if let Some(id) = c.get("id").filter(|v| !v.is_null()) {
                        let id = id.as_str().ok_or_else(|| invalid("invalid call id"))?;
                        if !parts.id.is_empty() && parts.id != id {
                            return Err(invalid("conflicting call id"));
                        }
                        if parts.id.is_empty() {
                            append(&mut parts.id, id, 256)?;
                        }
                    }
                    if let Some(function) = c.get("function") {
                        if let Some(name) = function.get("name").filter(|v| !v.is_null()) {
                            append(
                                &mut parts.name,
                                name.as_str().ok_or_else(|| invalid("invalid tool name"))?,
                                128,
                            )?;
                        }
                        if let Some(args) = function.get("arguments").filter(|v| !v.is_null()) {
                            let args = args
                                .as_str()
                                .ok_or_else(|| invalid("invalid argument fragment"))?;
                            append(&mut parts.arguments, args, MAX_ARGUMENT_BYTES)?;
                            self.arguments_delta(index + 1, args, on_event)?;
                        }
                    }
                }
            }
        }
        if let Some(finish) = choice.get("finish_reason").filter(|v| !v.is_null()) {
            if self.terminal.is_some() {
                return Err(invalid("multiple terminal outcomes"));
            }
            self.stop_reason = Some(
                finish
                    .as_str()
                    .ok_or_else(|| invalid("invalid finish reason"))?
                    .to_owned(),
            );
            self.terminal = Some(Value::Null);
        }
        Ok(())
    }

    fn responses(
        &mut self,
        value: &Value,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        let kind = string(value, "type")?;
        let index = value
            .get("output_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if index >= MAX_BLOCKS {
            return Err(invalid("excessive output index"));
        }
        let part = value
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        if part >= MAX_BLOCKS {
            return Err(invalid("excessive content index"));
        }
        match kind {
            "response.output_text.delta" => {
                self.text_delta(index * MAX_BLOCKS + part, string(value, "delta")?, on_event)
            }
            "response.function_call_arguments.delta" => {
                if self.argument_parts.len() >= MAX_TOOL_CALLS
                    && !self.argument_parts.contains_key(&index)
                {
                    return Err(invalid("too many streamed calls"));
                }
                let delta = string(value, "delta")?;
                append(
                    self.argument_parts.entry(index).or_default(),
                    delta,
                    MAX_ARGUMENT_BYTES,
                )?;
                self.arguments_delta(index * MAX_BLOCKS, delta, on_event)
            }
            "response.completed" | "response.incomplete" => {
                if self.terminal.is_some() {
                    return Err(invalid("multiple terminal outcomes"));
                }
                let response = value
                    .get("response")
                    .ok_or_else(|| invalid("terminal event lacks response"))?;
                let expected = if kind == "response.completed" {
                    "completed"
                } else {
                    "incomplete"
                };
                if string(response, "status")? != expected {
                    return Err(invalid("contradictory response outcome"));
                }
                self.usage.merge(usage(response));
                self.terminal = Some(response.clone());
                Ok(())
            }
            "response.created"
            | "response.in_progress"
            | "response.queued"
            | "response.output_item.added"
            | "response.output_item.done"
            | "response.content_part.added"
            | "response.content_part.done"
            | "response.output_text.done"
            | "response.function_call_arguments.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.delta"
            | "response.reasoning_text.done"
            | "response.refusal.delta"
            | "response.refusal.done" => {
                if self.terminal.is_some() {
                    return Err(invalid("semantic event after terminal"));
                }
                if let Some(item) = value.get("item")
                    && !matches!(
                        item.get("type").and_then(Value::as_str),
                        Some("message" | "function_call" | "reasoning")
                    )
                {
                    return Err(unsupported());
                }
                Ok(())
            }
            _ => Err(unsupported()),
        }
    }

    fn messages(
        &mut self,
        value: &Value,
        on_event: &mut (dyn FnMut(ProviderEvent) + Send),
    ) -> Result<()> {
        if self.terminal.is_some() {
            return Err(invalid("semantic event after terminal"));
        }
        let kind = string(value, "type")?;
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if index >= MAX_BLOCKS {
            return Err(invalid("excessive content index"));
        }
        match kind {
            "message_start" => {
                if let Some(message) = value.get("message") {
                    self.usage.merge(usage(message));
                    if message
                        .get("content")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty())
                    {
                        return Err(unsupported());
                    }
                }
                Ok(())
            }
            "content_block_start" => {
                if self.anthropic.contains_key(&index) {
                    return Err(invalid("duplicate content block"));
                }
                let content = value
                    .get("content_block")
                    .cloned()
                    .ok_or_else(|| invalid("missing content block"))?;
                if !matches!(
                    content.get("type").and_then(Value::as_str),
                    Some("text" | "tool_use" | "thinking" | "redacted_thinking")
                ) {
                    return Err(unsupported());
                }
                if content.get("type").and_then(Value::as_str) == Some("text") {
                    self.text_delta(index, string(&content, "text")?, on_event)?;
                }
                self.anthropic.insert(index, content);
                Ok(())
            }
            "content_block_delta" => {
                if self.closed.contains(&index) {
                    return Err(invalid("delta after completed block"));
                }
                let delta = value
                    .get("delta")
                    .ok_or_else(|| invalid("missing block delta"))?;
                let block = self
                    .anthropic
                    .get_mut(&index)
                    .ok_or_else(|| invalid("delta without content block"))?;
                let (field, key, expected) = match string(delta, "type")? {
                    "text_delta" => ("text", "text", "text"),
                    "input_json_delta" => ("input", "partial_json", "tool_use"),
                    "thinking_delta" => ("thinking", "thinking", "thinking"),
                    "signature_delta" => ("signature", "signature", "thinking"),
                    _ => return Err(unsupported()),
                };
                if string(block, "type")? != expected {
                    return Err(invalid("delta contradicts block type"));
                }
                let text = string(delta, key)?;
                if field == "input" {
                    append(
                        self.argument_parts.entry(index).or_default(),
                        text,
                        MAX_ARGUMENT_BYTES,
                    )?;
                    self.arguments_delta(index, text, on_event)?;
                } else {
                    let mut current = block
                        .get(field)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    append(&mut current, text, super::opencode::MAX_STREAM_TEXT_BYTES)?;
                    block[field] = current.into();
                    if field == "text" {
                        self.text_delta(index, text, on_event)?;
                    }
                }
                Ok(())
            }
            "content_block_stop" => {
                if !self.anthropic.contains_key(&index) {
                    return Err(invalid("stop without block"));
                }
                if !self.closed.insert(index) {
                    return Err(invalid("duplicate block stop"));
                }
                Ok(())
            }
            "message_delta" => {
                let delta = value
                    .get("delta")
                    .ok_or_else(|| invalid("missing message delta"))?;
                if let Some(reason) = delta.get("stop_reason").filter(|v| !v.is_null()) {
                    self.stop_reason = Some(
                        reason
                            .as_str()
                            .ok_or_else(|| invalid("invalid stop reason"))?
                            .to_owned(),
                    );
                }
                Ok(())
            }
            "message_stop" => {
                if self.closed.len() != self.anthropic.len() {
                    return Err(invalid("unfinished message blocks"));
                }
                self.terminal = Some(Value::Null);
                Ok(())
            }
            "ping" => Ok(()),
            _ => Err(unsupported()),
        }
    }

    pub(super) fn finish(
        mut self,
        request: &InferenceRequest,
        provider: ProviderId,
    ) -> Result<InferenceResponse> {
        let terminal = self
            .terminal
            .ok_or_else(|| invalid("stream ended before terminal evidence"))?;
        if self.wire == WireProtocol::OpenAiChatCompletions
            && self.calls.keys().enumerate().any(|(i, index)| i != *index)
        {
            return Err(invalid("noncontiguous tool-call indexes"));
        }
        if self.wire == WireProtocol::AnthropicMessages
            && self
                .anthropic
                .keys()
                .enumerate()
                .any(|(i, index)| i != *index)
        {
            return Err(invalid("noncontiguous content indexes"));
        }
        if self.wire == WireProtocol::AnthropicMessages
            && self.stop_reason.as_deref() == Some("tool_use")
        {
            for (index, args) in self.argument_parts {
                self.anthropic
                    .get_mut(&index)
                    .ok_or_else(|| invalid("arguments without block"))?["input"] =
                    serde_json::from_str(&args)
                        .map_err(|_| invalid("invalid completed tool arguments"))?;
            }
        }
        let mut value = match self.wire {
            WireProtocol::OpenAiChatCompletions => {
                let calls:Vec<_>=self.calls.into_values().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments}})).collect();
                json!({"choices":[{"finish_reason":self.stop_reason,"message":{"role":"assistant","content":self.text,"tool_calls":calls,"reasoning_content":self.reasoning,"refusal":if self.refusal.is_empty(){Value::Null}else{self.refusal.into()}}}]})
            }
            WireProtocol::OpenAiResponses => terminal,
            WireProtocol::AnthropicMessages => {
                json!({"content":self.anthropic.into_values().collect::<Vec<_>>(),"stop_reason":self.stop_reason})
            }
        };
        if value.get("model").is_none()
            && let Some(model) = self.model
        {
            value["model"] = model.into();
        }
        let mut response = decode(&value, self.wire, request, provider)?;
        response.usage.merge(self.usage);
        Ok(response)
    }
}
