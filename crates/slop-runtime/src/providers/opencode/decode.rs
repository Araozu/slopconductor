use super::*;

/// Parse the OpenAI-style model list into advertised ids.
pub(in crate::providers) fn parse_models_list(body: &str) -> Result<Vec<String>, DecodeError> {
    let value: Value = serde_json::from_str(body).map_err(|_| DecodeError::InvalidResponse {
        detail: "model list is not JSON",
    })?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or(DecodeError::InvalidResponse {
            detail: "model list has no data array",
        })?;
    let mut ids = Vec::with_capacity(data.len());
    for entry in data {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or(DecodeError::InvalidResponse {
                detail: "model entry has no id",
            })?;
        if !is_valid_model_id(id) {
            return Err(invalid_terminal("model list contains an invalid model id"));
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}

pub(super) fn incomplete(reason: Option<&str>) -> TurnOutcome {
    TurnOutcome::Incomplete {
        reason: match reason {
            Some("max_tokens") => "max_tokens",
            Some("max_output_tokens") => "max_output_tokens",
            Some("content_filter") => "content_filter",
            _ => "unknown",
        }
        .to_owned(),
    }
}

pub(super) fn invalid_terminal(detail: &'static str) -> DecodeError {
    DecodeError::InvalidResponse { detail }
}

pub(in crate::providers) fn reported_model(value: &Value) -> Result<Option<String>, DecodeError> {
    let model = value
        .get("model")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("model"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("model"))
        });
    match model {
        None => Ok(None),
        Some(Value::String(id)) if is_valid_model_id(id) => Ok(Some(id.clone())),
        Some(_) => Err(invalid_terminal("response contains an invalid model id")),
    }
}

pub(super) fn chat_outcome(finish_reason: Option<&str>) -> Result<TurnOutcome, DecodeError> {
    match finish_reason {
        Some("stop") => Ok(TurnOutcome::Completed),
        Some("length") => Ok(incomplete(Some("max_tokens"))),
        Some("content_filter") => Ok(incomplete(Some("content_filter"))),
        Some("tool_calls" | "function_call") => Err(DecodeError::UnsupportedCapability {
            capability: "tool proposals",
        }),
        _ => Err(invalid_terminal("missing or unknown chat finish reason")),
    }
}

pub(super) fn messages_outcome(stop_reason: Option<&str>) -> Result<TurnOutcome, DecodeError> {
    match stop_reason {
        Some("end_turn" | "stop_sequence") => Ok(TurnOutcome::Completed),
        Some("max_tokens") => Ok(incomplete(Some("max_tokens"))),
        Some("tool_use" | "pause_turn" | "refusal") => Err(DecodeError::UnsupportedCapability {
            capability: "structured message outcome",
        }),
        _ => Err(invalid_terminal("missing or unknown messages stop reason")),
    }
}

/// Parse a Chat Completions response into `(text, usage, outcome)`.
pub(in crate::providers) fn parse_chat_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    if has_content(body.get("error")) {
        return Err(DecodeError::TurnFailed);
    }
    let choices = body
        .get("choices")
        .and_then(Value::as_array)
        .ok_or(invalid("chat response has no choices"))?;
    if choices.len() > 1 {
        return Err(unsupported_content());
    }
    let choice = choices
        .first()
        .ok_or(invalid("chat response has no choices"))?;
    let message = choice
        .get("message")
        .ok_or(invalid("chat choice has no message"))?;
    validate_chat_content(message)?;
    let text = match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        _ => return Err(unsupported_content()),
    };
    let finish = choice.get("finish_reason").and_then(Value::as_str);
    Ok((text, parse_openai_usage(body), chat_outcome(finish)?))
}

/// Parse a Responses response into `(text, usage, outcome)`.
///
/// Text comes from `message` items' `output_text` parts; unsupported structured
/// output is rejected instead of silently discarded. A `failed` status
/// is an error, never an empty success; `incomplete` (usually
/// `max_output_tokens` too small for the model's reasoning effort) keeps its
/// reason alongside partial text and usage.
pub(in crate::providers) fn parse_responses_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    match body.get("status").and_then(Value::as_str) {
        Some("failed") => {
            return Err(DecodeError::TurnFailed);
        }
        Some("completed" | "incomplete") => {}
        _ => return Err(invalid("missing or unexpected responses status")),
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or(invalid("responses body has no output"))?;
    let mut text = String::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {}
            Some("reasoning")
                if !has_content(item.get("summary"))
                    && !has_content(item.get("encrypted_content")) =>
            {
                continue;
            }
            _ => return Err(unsupported_content()),
        }
        let content = item
            .get("content")
            .and_then(Value::as_array)
            .ok_or(invalid("responses message has no content"))?;
        for part in content {
            if part.get("type").and_then(Value::as_str) != Some("output_text") {
                return Err(unsupported_content());
            }
            if has_content(part.get("annotations")) {
                return Err(unsupported_content());
            }
            let chunk = part
                .get("text")
                .and_then(Value::as_str)
                .ok_or(invalid("output text has no text string"))?;
            text.push_str(chunk);
        }
    }
    let outcome = match body.get("status").and_then(Value::as_str) {
        Some("incomplete") => incomplete(
            body.get("incomplete_details")
                .and_then(|d| d.get("reason"))
                .and_then(Value::as_str),
        ),
        _ => TurnOutcome::Completed,
    };
    Ok((text, parse_openai_usage(body), outcome))
}

/// Parse an Anthropic Messages response into `(text, usage, outcome)`.
pub(in crate::providers) fn parse_messages_response(
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let invalid = |detail| DecodeError::InvalidResponse { detail };
    if body.get("type").and_then(Value::as_str) == Some("error") {
        return Err(DecodeError::TurnFailed);
    }
    let content = body
        .get("content")
        .and_then(Value::as_array)
        .ok_or(invalid("messages body has no content"))?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            return Err(unsupported_content());
        }
        if has_content(block.get("citations")) {
            return Err(unsupported_content());
        }
        let chunk = block
            .get("text")
            .and_then(Value::as_str)
            .ok_or(invalid("text block has no text string"))?;
        text.push_str(chunk);
    }
    let outcome = messages_outcome(body.get("stop_reason").and_then(Value::as_str))?;
    Ok((text, parse_openai_usage(body), outcome))
}

pub(super) fn parse_openai_usage(body: &Value) -> Usage {
    let Some(usage) = body.get("usage") else {
        return Usage::default();
    };
    parse_usage(usage)
}

pub(super) fn parse_usage(usage: &Value) -> Usage {
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64);
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64);
    let total = usage.get("total_tokens").and_then(Value::as_u64);
    Usage::from_reported(input, output, total)
}

pub(super) fn unsupported_content() -> DecodeError {
    DecodeError::UnsupportedCapability {
        capability: "structured response content in the text-only adapter",
    }
}

pub(super) fn has_content(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

pub(super) fn validate_chat_content(content: &Value) -> Result<(), DecodeError> {
    if ["tool_calls", "function_call", "refusal", "audio"]
        .iter()
        .any(|field| has_content(content.get(field)))
    {
        return Err(unsupported_content());
    }
    if content
        .get("content")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(unsupported_content());
    }
    Ok(())
}

pub(super) fn validate_stream_content(
    value: &Value,
    wire: WireProtocol,
) -> Result<(), DecodeError> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let choices = value
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_terminal("chat stream event has no choices"))?;
            if choices.len() > 1 {
                return Err(unsupported_content());
            }
            if let Some(delta) = choices.first().and_then(|choice| choice.get("delta")) {
                validate_chat_content(delta)?;
            }
        }
        WireProtocol::AnthropicMessages => match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let block = value
                    .get("content_block")
                    .ok_or_else(|| invalid_terminal("block start has no content block"))?;
                if block.get("type").and_then(Value::as_str) != Some("text")
                    || has_content(block.get("citations"))
                {
                    return Err(unsupported_content());
                }
                if !block.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text block has no text string"));
                }
            }
            Some("content_block_delta") => {
                let delta = value
                    .get("delta")
                    .ok_or_else(|| invalid_terminal("block delta has no delta"))?;
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    return Err(unsupported_content());
                }
                if !delta.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text delta has no text string"));
                }
            }
            Some("message_start") => {
                if value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .is_some_and(|content| !content.is_array() || has_content(Some(content)))
                {
                    return Err(invalid_terminal("message start content is not empty"));
                }
            }
            Some("message_delta" | "message_stop" | "content_block_stop" | "ping") => {}
            _ => return Err(invalid_terminal("unsupported messages stream event")),
        },
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => {
                let item = value
                    .get("item")
                    .ok_or_else(|| invalid_terminal("output item event has no item"))?;
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        parse_responses_response(&serde_json::json!({
                            "status": "completed", "output": [item]
                        }))?;
                    }
                    Some("reasoning")
                        if !has_content(item.get("summary"))
                            && !has_content(item.get("encrypted_content")) => {}
                    _ => return Err(unsupported_content()),
                }
            }
            Some("response.content_part.added" | "response.content_part.done") => {
                let part = value
                    .get("part")
                    .ok_or_else(|| invalid_terminal("content part event has no part"))?;
                if part.get("type").and_then(Value::as_str) != Some("output_text")
                    || has_content(part.get("annotations"))
                {
                    return Err(unsupported_content());
                }
                if !part.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("output text part has no text string"));
                }
            }
            Some("response.output_text.delta") => {
                if !value.get("delta").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("text delta has no text string"));
                }
            }
            Some("response.output_text.done") => {
                if !value.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal("completed text event has no text string"));
                }
            }
            Some(
                "response.created"
                | "response.in_progress"
                | "response.completed"
                | "response.incomplete",
            ) => {}
            _ => return Err(unsupported_content()),
        },
    }
    Ok(())
}
