use serde_json::{Value, json};
use slop_core::provider::ProviderId;

use super::super::{WireProtocol, inference::*};
use super::common::{role, tool_output};

pub(in crate::providers) fn encode(
    request: &InferenceRequest,
    wire: WireProtocol,
    provider: ProviderId,
    scope: &str,
) -> std::result::Result<Value, super::super::ProviderError> {
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
            if let Some(limit) = request.settings.max_output_tokens {
                body["max_output_tokens"] = limit.into();
            }
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
                return Err(super::super::ProviderError::UnsupportedCapability {
                    capability: "settings.reasoning_effort",
                });
            }
        }
    }
    Ok(body)
}
