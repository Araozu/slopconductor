use serde_json::Value;
use slop_core::provider::ProviderId;

use super::super::{WireProtocol, inference::*, opencode::DecodeError};
use super::common::{Result, array, block, call, invalid, string, unsupported, usage};

pub(in crate::providers) fn decode(
    value: &Value,
    wire: WireProtocol,
    request: &InferenceRequest,
    provider: ProviderId,
) -> Result<InferenceResponse> {
    if super::super::transport::is_context_error(value) {
        return Err(DecodeError::ContextLengthExceeded);
    }
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
                "length" => FinishReason::OutputLimit,
                "content_filter" => FinishReason::ContentFilter,
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
                        if item.get("role").is_some_and(|role| role != "assistant")
                            || status == "completed"
                                && item.get("status").is_some_and(|s| s != "completed")
                        {
                            return Err(invalid("invalid completed assistant item"));
                        }
                        // Preserve assistant phase and output identity when
                        // manually replaying Responses history.
                        if item.get("phase").is_some_and(|phase| !phase.is_null()) {
                            private = true;
                        }
                        for (part_index, part) in array(item, "content")?.iter().enumerate() {
                            if part_index >= MAX_BLOCKS {
                                return Err(invalid("too many content parts"));
                            }
                            let content = match string(part, "type")? {
                                "output_text" => BlockContent::Text {
                                    text: string(part, "text")?.to_owned(),
                                },
                                "refusal" => {
                                    // Replay the original output item so a
                                    // refusal does not become ordinary text.
                                    private = true;
                                    BlockContent::Refusal {
                                        text: string(part, "refusal")?.to_owned(),
                                    }
                                }
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
                match value
                    .get("incomplete_details")
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                {
                    Some("max_output_tokens") => FinishReason::OutputLimit,
                    Some("content_filter") => FinishReason::ContentFilter,
                    _ => FinishReason::Incomplete,
                }
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
                "model_context_window_exceeded" => FinishReason::ContextLimit,
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
                    "tool_use"
                        if matches!(
                            finish,
                            FinishReason::OutputLimit | FinishReason::ContextLimit
                        ) => {}
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
            > super::super::opencode::MAX_BODY_BYTES
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
    if text_bytes > super::super::opencode::MAX_STREAM_TEXT_BYTES {
        return Err(invalid("visible output exceeds bound"));
    }
    let continuation = if private_items.is_empty()
        || matches!(
            finish_reason,
            FinishReason::OutputLimit
                | FinishReason::ContextLimit
                | FinishReason::Incomplete
                | FinishReason::ContentFilter
        ) {
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
        resolved_model: super::super::opencode::reported_model(value)?,
        message: InferenceMessage {
            role: MessageRole::Assistant,
            blocks,
            continuation,
        },
        usage: usage(value),
        finish_reason,
    })
}
