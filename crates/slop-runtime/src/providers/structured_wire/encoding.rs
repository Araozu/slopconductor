use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
    let call_ids = WireCallIds::new(request);
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
                    messages.extend(call_ids.replay(message, &original.items, wire));
                    continue;
                }
                if message.role == MessageRole::Tool {
                    for block in &message.blocks {
                        if let BlockContent::ToolResult {
                            call_id,
                            provider_call_id,
                            ..
                        } = &block.content
                        {
                            messages.push(json!({"role":"tool","tool_call_id":call_ids.id(call_id, provider_call_id),"content":tool_output(&block.content)}));
                        }
                    }
                    continue;
                }
                let mut value =
                    json!({"role":role(&message.role),"content":message.visible_text()});
                if message.role == MessageRole::Assistant {
                    let text: String = message
                        .blocks
                        .iter()
                        .filter_map(|block| match &block.content {
                            BlockContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    let refusal: String = message
                        .blocks
                        .iter()
                        .filter_map(|block| match &block.content {
                            BlockContent::Refusal { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect();
                    value["content"] = text.into();
                    if !refusal.is_empty() {
                        value["refusal"] = refusal.into();
                    }
                }
                let calls: Vec<_> = message.blocks.iter().filter_map(|b| match &b.content {
                    BlockContent::ToolCall { call_id, provider_call_id, name, arguments } => Some(json!({"id":call_ids.id(call_id, provider_call_id),"type":"function","function":{"name":name,"arguments":arguments.to_string()}})),
                    _ => None,
                }).collect();
                if !calls.is_empty() {
                    value["tool_calls"] = calls.into();
                }
                messages.push(value);
            }
            WireProtocol::OpenAiResponses => {
                if let Some(original) = original {
                    messages.extend(call_ids.replay(message, &original.items, wire));
                    continue;
                }
                for block in &message.blocks {
                    messages.push(match &block.content {
                        BlockContent::Text { text } | BlockContent::Refusal { text } => json!({"role":role(&message.role),"content":text}),
                        BlockContent::ToolCall { call_id, provider_call_id, name, arguments } => json!({"type":"function_call","call_id":call_ids.id(call_id, provider_call_id),"name":name,"arguments":arguments.to_string()}),
                        BlockContent::ToolResult { call_id, provider_call_id, .. } => json!({"type":"function_call_output","call_id":call_ids.id(call_id, provider_call_id),"output":tool_output(&block.content)}),
                    });
                }
            }
            WireProtocol::AnthropicMessages => {
                if message.role == MessageRole::System {
                    system = Some(message.visible_text());
                    continue;
                }
                let content = if let Some(original) = original {
                    call_ids.replay(message, &original.items, wire)
                } else {
                    message.blocks.iter().map(|block| match &block.content {
                        BlockContent::Text { text } | BlockContent::Refusal { text } => json!({"type":"text","text":text}),
                        BlockContent::ToolCall { call_id, provider_call_id, name, arguments } => json!({"type":"tool_use","id":call_ids.id(call_id, provider_call_id),"name":name,"input":arguments}),
                        BlockContent::ToolResult { call_id, provider_call_id, is_error, .. } => json!({"type":"tool_result","tool_use_id":call_ids.id(call_id, provider_call_id),"content":tool_output(&block.content),"is_error":is_error}),
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

/// Gateways may reuse a provider call ID on separate requests. Canonical call
/// identities are request-scoped, so use them to disambiguate replay without
/// changing the persisted provider metadata or opaque reasoning payloads.
struct WireCallIds<'a> {
    by_call: HashMap<&'a str, String>,
}

impl<'a> WireCallIds<'a> {
    fn new(request: &'a InferenceRequest) -> Self {
        let calls: Vec<_> = request
            .messages
            .iter()
            .flat_map(|message| &message.blocks)
            .filter_map(|block| match &block.content {
                BlockContent::ToolCall {
                    call_id,
                    provider_call_id,
                    ..
                } => Some((call_id.as_str(), provider_call_id.as_str())),
                _ => None,
            })
            .collect();
        let mut reserved: HashSet<_> = calls.iter().map(|(_, id)| (*id).to_owned()).collect();
        let mut seen = HashSet::new();
        let mut by_call = HashMap::new();
        for (call_id, provider_call_id) in calls {
            let id = if seen.insert(provider_call_id) {
                provider_call_id.to_owned()
            } else {
                let mut salt = 0u64;
                loop {
                    let mut hash = Sha256::new();
                    hash.update(call_id.as_bytes());
                    hash.update(salt.to_be_bytes());
                    let digest = format!("{:x}", hash.finalize());
                    let candidate = format!("call_slop_{}", &digest[..32]);
                    if reserved.insert(candidate.clone()) {
                        break candidate;
                    }
                    salt += 1;
                }
            };
            by_call.insert(call_id, id);
        }
        Self { by_call }
    }

    fn id<'b>(&'b self, call_id: &str, original: &'b str) -> &'b str {
        self.by_call
            .get(call_id)
            .map(String::as_str)
            .unwrap_or(original)
    }

    fn replay(
        &self,
        message: &InferenceMessage,
        items: &[Value],
        wire: WireProtocol,
    ) -> Vec<Value> {
        let replacements: HashMap<_, _> = message
            .blocks
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::ToolCall {
                    call_id,
                    provider_call_id,
                    ..
                } => Some((
                    provider_call_id.as_str(),
                    self.id(call_id, provider_call_id),
                )),
                _ => None,
            })
            .collect();
        let mut items = items.to_vec();
        for item in &mut items {
            match wire {
                WireProtocol::OpenAiChatCompletions => {
                    if let Some(calls) = item.get_mut("tool_calls").and_then(Value::as_array_mut) {
                        for call in calls {
                            Self::replace(call, "id", &replacements);
                        }
                    }
                }
                WireProtocol::OpenAiResponses if item["type"] == "function_call" => {
                    Self::replace(item, "call_id", &replacements);
                }
                WireProtocol::AnthropicMessages if item["type"] == "tool_use" => {
                    Self::replace(item, "id", &replacements);
                }
                _ => {}
            }
        }
        items
    }

    fn replace(item: &mut Value, field: &str, replacements: &HashMap<&str, &str>) {
        if let Some(id) = item.get(field).and_then(Value::as_str)
            && let Some(replacement) = replacements.get(id)
            && id != *replacement
        {
            item[field] = (*replacement).into();
        }
    }
}
