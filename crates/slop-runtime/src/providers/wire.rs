//! Shared, bounded decoding for the providers' HTTP wire protocols.

use serde_json::Value;
use slop_core::provider::{ProviderId, is_valid_model_id};

use super::{ChatResponse, ProviderError, Role, StreamDelta, TurnOutcome, Usage, WireProtocol};

pub(super) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
pub(super) const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
pub(super) const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn decode_response(
    provider: ProviderId,
    wire: WireProtocol,
    model: &str,
    value: &Value,
) -> Result<ChatResponse, ProviderError> {
    let (text, usage, outcome) = match wire {
        WireProtocol::OpenAiChatCompletions => parse_chat_response(provider, value)?,
        WireProtocol::OpenAiResponses => parse_responses_response(provider, value)?,
        WireProtocol::AnthropicMessages => parse_messages_response(provider, value)?,
    };
    Ok(ChatResponse {
        model: model.to_owned(),
        resolved_model: reported_model(provider, value)?,
        text,
        usage,
        wire,
        outcome,
    })
}

pub(super) fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"))
}

/// Bounded SSE event decoder that never splits UTF-8 across decodes.
///
/// CR/LF bytes cannot appear inside a multi-byte UTF-8 sequence. Decode only
/// complete lines, accepting LF, CRLF and CR even across network chunks.
pub(super) struct SseStream {
    provider: ProviderId,
    pending: Vec<u8>,
    event_data: Vec<String>,
    event_bytes: usize,
    total: usize,
}

impl SseStream {
    pub(super) fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            pending: Vec::new(),
            event_data: Vec::new(),
            event_bytes: 0,
            total: 0,
        }
    }

    /// Feed one network chunk; dispatch only blank-line-terminated events.
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, ProviderError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_STREAM_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_STREAM_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.drain_lines(false)
    }

    /// Drain complete lines, rejecting overlong lines instead of buffering
    /// them without bound.
    fn drain_lines(&mut self, eof: bool) -> Result<Vec<String>, ProviderError> {
        let provider = self.provider;
        let mut events = Vec::new();
        let mut consumed = 0;
        while let Some(offset) = self.pending[consumed..]
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
        {
            let delimiter = consumed + offset;
            if self.pending[delimiter] == b'\r' && delimiter + 1 == self.pending.len() && !eof {
                break;
            }
            let delimiter_bytes = if self.pending[delimiter] == b'\r'
                && self.pending.get(delimiter + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let end = delimiter + delimiter_bytes;
            if end - consumed > MAX_SSE_LINE_BYTES {
                return Err(ProviderError::LimitExceeded {
                    detail: "stream line exceeds byte bound",
                });
            }
            let line = std::str::from_utf8(&self.pending[consumed..end]).map_err(|_| {
                ProviderError::InvalidResponse {
                    provider,
                    detail: "stream is not valid UTF-8",
                }
            })?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.event_data.is_empty() {
                    events.push(self.event_data.join("\n"));
                    self.event_data.clear();
                    self.event_bytes = 0;
                }
            } else if line.starts_with("data:") {
                self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
                if self.event_bytes > MAX_SSE_LINE_BYTES {
                    return Err(ProviderError::LimitExceeded {
                        detail: "stream event exceeds byte bound",
                    });
                }
                self.event_data.push(line.to_owned());
            }
            consumed = end;
        }
        self.pending.drain(..consumed);
        if self.pending.len() > MAX_SSE_LINE_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "stream line exceeds byte bound",
            });
        }
        Ok(events)
    }

    /// EOF cannot dispatch an unterminated event as a completed response.
    pub(super) fn finish(&mut self) -> Result<Vec<String>, ProviderError> {
        let provider = self.provider;
        let events = self.drain_lines(true)?;
        if self.pending.is_empty() && self.event_data.is_empty() {
            return Ok(events);
        }
        Err(ProviderError::InvalidResponse {
            provider,
            detail: "stream ended inside an SSE event",
        })
    }
}

/// Translate neutral messages to OpenAI Chat `messages`.
pub(super) fn to_openai_messages(messages: &[super::ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role.as_str(),
                "content": m.content,
            })
        })
        .collect()
}

/// Translate neutral messages to a Responses `input` value.
///
/// A single user turn passes through as a plain string (the shape verified
/// live). Anything with roles the string form cannot express — system
/// instructions, assistant history, multi-turn context — becomes the
/// structured message array so role boundaries survive.
pub(super) fn to_responses_input(messages: &[super::ChatMessage]) -> Value {
    if messages.len() == 1 && messages[0].role == Role::User {
        return Value::String(messages[0].content.clone());
    }
    Value::Array(
        messages
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role.as_str(),
                    "content": m.content,
                })
            })
            .collect(),
    )
}

/// Split neutral messages into Anthropic `system` plus `messages`.
///
/// Validation permits one optional leading system prompt outside `messages`.
/// Other system placements are rejected before this conversion.
pub(super) fn to_anthropic_parts(messages: &[super::ChatMessage]) -> (Option<String>, Vec<Value>) {
    let mut system = Vec::new();
    let mut rest = Vec::new();
    for m in messages {
        match m.role {
            Role::System | Role::Developer => system.push(m.content.clone()),
            Role::User | Role::Assistant => rest.push(serde_json::json!({
                "role": m.role.as_str(),
                "content": m.content,
            })),
        }
    }
    let system = if system.is_empty() {
        None
    } else {
        Some(system.join("\n\n"))
    };
    (system, rest)
}

/// Parse the OpenAI-style model list into advertised ids.
pub(super) fn parse_models_list(
    provider: ProviderId,
    body: &str,
) -> Result<Vec<String>, ProviderError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ProviderError::InvalidResponse {
        provider,
        detail: "model list is not JSON",
    })?;
    let data =
        value
            .get("data")
            .and_then(Value::as_array)
            .ok_or(ProviderError::InvalidResponse {
                provider,
                detail: "model list has no data array",
            })?;
    let mut ids = Vec::with_capacity(data.len());
    for entry in data {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ProviderError::InvalidResponse {
                provider,
                detail: "model entry has no id",
            })?;
        if !is_valid_model_id(id) {
            return Err(invalid_terminal(
                provider,
                "model list contains an invalid model id",
            ));
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

pub(super) fn invalid_terminal(provider: ProviderId, detail: &'static str) -> ProviderError {
    ProviderError::InvalidResponse { provider, detail }
}

pub(super) fn reported_model(
    provider: ProviderId,
    value: &Value,
) -> Result<Option<String>, ProviderError> {
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
        Some(_) => Err(invalid_terminal(
            provider,
            "response contains an invalid model id",
        )),
    }
}

pub(super) fn chat_outcome(
    provider: ProviderId,
    finish_reason: Option<&str>,
) -> Result<TurnOutcome, ProviderError> {
    match finish_reason {
        Some("stop") => Ok(TurnOutcome::Completed),
        Some("length") => Ok(incomplete(Some("max_tokens"))),
        Some("content_filter") => Ok(incomplete(Some("content_filter"))),
        Some("tool_calls" | "function_call") => Err(ProviderError::UnsupportedCapability {
            capability: "tool proposals",
        }),
        _ => Err(invalid_terminal(
            provider,
            "missing or unknown chat finish reason",
        )),
    }
}

pub(super) fn messages_outcome(
    provider: ProviderId,
    stop_reason: Option<&str>,
) -> Result<TurnOutcome, ProviderError> {
    match stop_reason {
        Some("end_turn" | "stop_sequence") => Ok(TurnOutcome::Completed),
        Some("max_tokens") => Ok(incomplete(Some("max_tokens"))),
        Some("tool_use" | "pause_turn" | "refusal") => Err(ProviderError::UnsupportedCapability {
            capability: "structured message outcome",
        }),
        _ => Err(invalid_terminal(
            provider,
            "missing or unknown messages stop reason",
        )),
    }
}

/// Parse a Chat Completions response into `(text, usage, outcome)`.
pub(super) fn parse_chat_response(
    provider: ProviderId,
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse { provider, detail };
    if has_content(body.get("error")) {
        return Err(ProviderError::TurnFailed { provider });
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
    Ok((
        text,
        parse_openai_usage(body),
        chat_outcome(provider, finish)?,
    ))
}

/// Parse a Responses response into `(text, usage, outcome)`.
///
/// Text comes from `message` items' `output_text` parts; unsupported structured
/// output is rejected instead of silently discarded. A `failed` status
/// is an error, never an empty success; `incomplete` (usually
/// `max_output_tokens` too small for the model's reasoning effort) keeps its
/// reason alongside partial text and usage.
pub(super) fn parse_responses_response(
    provider: ProviderId,
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse { provider, detail };
    match body.get("status").and_then(Value::as_str) {
        Some("failed") => {
            return Err(ProviderError::TurnFailed { provider });
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
pub(super) fn parse_messages_response(
    provider: ProviderId,
    body: &Value,
) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let invalid = |detail| ProviderError::InvalidResponse { provider, detail };
    if body.get("type").and_then(Value::as_str) == Some("error") {
        return Err(ProviderError::TurnFailed { provider });
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
    let outcome = messages_outcome(provider, body.get("stop_reason").and_then(Value::as_str))?;
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

pub(super) fn unsupported_content() -> ProviderError {
    ProviderError::UnsupportedCapability {
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

pub(super) fn validate_chat_content(content: &Value) -> Result<(), ProviderError> {
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
    provider: ProviderId,
    value: &Value,
    wire: WireProtocol,
) -> Result<(), ProviderError> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let choices = value
                .get("choices")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_terminal(provider, "chat stream event has no choices"))?;
            if choices.len() > 1 {
                return Err(unsupported_content());
            }
            if let Some(delta) = choices.first().and_then(|choice| choice.get("delta")) {
                validate_chat_content(delta)?;
            }
        }
        WireProtocol::AnthropicMessages => match value.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let block = value.get("content_block").ok_or_else(|| {
                    invalid_terminal(provider, "block start has no content block")
                })?;
                if block.get("type").and_then(Value::as_str) != Some("text")
                    || has_content(block.get("citations"))
                {
                    return Err(unsupported_content());
                }
                if !block.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal(provider, "text block has no text string"));
                }
            }
            Some("content_block_delta") => {
                let delta = value
                    .get("delta")
                    .ok_or_else(|| invalid_terminal(provider, "block delta has no delta"))?;
                if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
                    return Err(unsupported_content());
                }
                if !delta.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal(provider, "text delta has no text string"));
                }
            }
            Some("message_start") => {
                if value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .is_some_and(|content| !content.is_array() || has_content(Some(content)))
                {
                    return Err(invalid_terminal(
                        provider,
                        "message start content is not empty",
                    ));
                }
            }
            Some("message_delta" | "message_stop" | "content_block_stop" | "ping") => {}
            _ => {
                return Err(invalid_terminal(
                    provider,
                    "unsupported messages stream event",
                ));
            }
        },
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.output_item.added" | "response.output_item.done") => {
                let item = value
                    .get("item")
                    .ok_or_else(|| invalid_terminal(provider, "output item event has no item"))?;
                match item.get("type").and_then(Value::as_str) {
                    Some("message") => {
                        parse_responses_response(
                            provider,
                            &serde_json::json!({
                                "status": "completed", "output": [item]
                            }),
                        )?;
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
                    .ok_or_else(|| invalid_terminal(provider, "content part event has no part"))?;
                if part.get("type").and_then(Value::as_str) != Some("output_text")
                    || has_content(part.get("annotations"))
                {
                    return Err(unsupported_content());
                }
                if !part.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal(
                        provider,
                        "output text part has no text string",
                    ));
                }
            }
            Some("response.output_text.delta") => {
                if !value.get("delta").is_some_and(Value::is_string) {
                    return Err(invalid_terminal(provider, "text delta has no text string"));
                }
            }
            Some("response.output_text.done") => {
                if !value.get("text").is_some_and(Value::is_string) {
                    return Err(invalid_terminal(
                        provider,
                        "completed text event has no text string",
                    ));
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

/// Incremental fold of SSE lines into text, usage, and terminal outcome.
///
/// Shared by the async streaming loop and the synchronous test driver so both
/// enforce the same terminal, error, usage, and bound behavior.
#[derive(Debug)]
pub(super) struct StreamFold {
    provider: ProviderId,
    pub(super) text: String,
    pub(super) resolved_model: Option<String>,
    pub(super) usage: Usage,
    stop_reason: Option<String>,
    pub(super) terminal: Option<TurnOutcome>,
}

impl StreamFold {
    pub(super) fn new(provider: ProviderId) -> Self {
        Self {
            provider,
            text: String::new(),
            resolved_model: None,
            usage: Usage::default(),
            stop_reason: None,
            terminal: None,
        }
    }

    pub(super) fn feed(
        &mut self,
        line: &str,
        wire: WireProtocol,
        on_delta: &mut impl FnMut(StreamDelta),
    ) -> Result<(), ProviderError> {
        let provider = self.provider;
        let payload = sse_payload(line);
        if payload.trim() == "[DONE]" {
            if wire != WireProtocol::OpenAiChatCompletions || self.terminal.is_none() {
                return Err(invalid_terminal(
                    provider,
                    "end marker without a valid terminal outcome",
                ));
            }
            return Ok(());
        }
        let value: Value = serde_json::from_str(&payload)
            .map_err(|_| invalid_terminal(provider, "stream event is not valid JSON"))?;
        if sse_stream_error(&value) {
            return Err(ProviderError::TurnFailed { provider });
        }
        validate_stream_content(provider, &value, wire)?;
        if let Some(model) = reported_model(provider, &value)? {
            self.resolved_model = Some(model);
        }
        if let Some(reason) = sse_stop_reason(&value) {
            self.stop_reason = Some(reason);
        }
        if let Some(delta) = parse_stream_delta(&value, wire) {
            if self.terminal.is_some() {
                return Err(invalid_terminal(
                    provider,
                    "content received after terminal outcome",
                ));
            }
            if self.text.len().saturating_add(delta.text.len()) > MAX_STREAM_TEXT_BYTES {
                return Err(ProviderError::LimitExceeded {
                    detail: "streamed text exceeds byte bound",
                });
            }
            if !delta.text.is_empty() {
                self.text.push_str(&delta.text);
            }
            on_delta(delta);
        }
        merge_sse_usage(&mut self.usage, &value);
        if let Some(outcome) = sse_terminal(provider, &value, wire, self.stop_reason.as_deref())? {
            if self
                .terminal
                .as_ref()
                .is_some_and(|prior| *prior != outcome)
            {
                return Err(invalid_terminal(provider, "conflicting terminal outcomes"));
            }
            if wire == WireProtocol::OpenAiResponses {
                let (text, usage, _) = parse_responses_response(
                    provider,
                    value.get("response").ok_or_else(|| {
                        invalid_terminal(provider, "terminal event has no response")
                    })?,
                )?;
                if text.len() > MAX_STREAM_TEXT_BYTES {
                    return Err(ProviderError::LimitExceeded {
                        detail: "terminal text exceeds byte bound",
                    });
                }
                self.text = text;
                self.usage.merge(usage);
            }
            self.terminal.get_or_insert(outcome);
        }
        Ok(())
    }
}

/// SSE joins multiple data fields with newlines before JSON decoding.
pub(super) fn sse_payload(event: &str) -> String {
    event
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether one SSE line reports a provider-side failure.
///
/// Server error text is deliberately not propagated: the fixed
/// [`ProviderError::TurnFailed`] carries no upstream excerpt, so reflected
/// credentials cannot escape through streaming errors either.
pub(super) fn sse_stream_error(value: &Value) -> bool {
    has_content(value.get("error"))
        || matches!(
            value.get("type").and_then(Value::as_str),
            Some("error" | "response.failed")
        )
}

/// Capture a stop/finish reason carried by one SSE line, if any.
pub(super) fn sse_stop_reason(value: &Value) -> Option<String> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("delta")
                .and_then(|delta| delta.get("stop_reason"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned)
}

/// Whether one SSE line is the wire's terminal event, with its outcome.
pub(super) fn sse_terminal(
    provider: ProviderId,
    value: &Value,
    wire: WireProtocol,
    stop_reason: Option<&str>,
) -> Result<Option<TurnOutcome>, ProviderError> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let finish = value
                .get("choices")
                .and_then(Value::as_array)
                .and_then(|choices| choices.first())
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(Value::as_str);
            // A null finish reason marks an in-progress delta, not a terminal
            // event; only an explicit reason string terminates the stream.
            finish
                .map(|reason| chat_outcome(provider, Some(reason)))
                .transpose()
        }
        WireProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                messages_outcome(provider, stop_reason).map(Some)
            } else {
                Ok(None)
            }
        }
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.completed" | "response.incomplete") => {
                let response = value
                    .get("response")
                    .ok_or_else(|| invalid_terminal(provider, "terminal event has no response"))?;
                let (_, _, outcome) = parse_responses_response(provider, response)?;
                let expected =
                    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
                        "completed"
                    } else {
                        "incomplete"
                    };
                if response.get("status").and_then(Value::as_str) != Some(expected) {
                    return Err(invalid_terminal(
                        provider,
                        "terminal event contradicts response status",
                    ));
                }
                Ok(Some(outcome))
            }
            _ => Ok(None),
        },
    }
}

/// Extract a streaming text/reasoning delta from one SSE line.
pub(super) fn parse_stream_delta(value: &Value, wire: WireProtocol) -> Option<StreamDelta> {
    match wire {
        WireProtocol::OpenAiChatCompletions => {
            let delta = value.get("choices")?.as_array()?.first()?.get("delta")?;
            let text = delta
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let reasoning = delta
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() && reasoning.is_empty() {
                return None;
            }
            Some(StreamDelta { text, reasoning })
        }
        WireProtocol::AnthropicMessages => {
            let block = match value.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => value.get("delta")?,
                Some("content_block_start") => value.get("content_block")?,
                _ => return None,
            };
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            Some(StreamDelta {
                text,
                reasoning: String::new(),
            })
        }
        WireProtocol::OpenAiResponses => {
            if value.get("type").and_then(Value::as_str) != Some("response.output_text.delta") {
                return None;
            }
            let text = value
                .get("delta")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            if text.is_empty() {
                return None;
            }
            Some(StreamDelta {
                text,
                reasoning: String::new(),
            })
        }
    }
}

/// Merge cumulative usage without turning absent fields into reported zero.
///
/// Standard Anthropic streams announce input tokens in `message_start` and
/// cumulative output in `message_delta`; Chat chunks and Responses terminal
/// events carry their own `usage`. Explicit zeros replace previous values.
pub(super) fn merge_sse_usage(usage: &mut Usage, value: &Value) {
    if let Some(seen) = value
        .get("usage")
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("usage"))
        })
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
        })
    {
        usage.merge(parse_usage(seen));
    }
}

#[cfg(test)]
pub(super) fn parse_sse_line(event: &str, wire: WireProtocol) -> Option<StreamDelta> {
    let value = serde_json::from_str(&sse_payload(event)).ok()?;
    parse_stream_delta(&value, wire)
}

/// Synchronous SSE driver over a complete body, mirroring the async loop's
/// per-line logic. Used by tests to cover terminal, error, usage, and bound
/// behavior without HTTP.
#[cfg(test)]
pub(super) fn process_sse_body(
    provider: ProviderId,
    body: &str,
    wire: WireProtocol,
) -> Result<(String, Usage, TurnOutcome), ProviderError> {
    let mut stream = SseStream::new(provider);
    let mut lines = stream.push(body.as_bytes())?;
    lines.extend(stream.finish()?);
    let mut fold = StreamFold::new(provider);
    for line in &lines {
        fold.feed(line, wire, &mut |_| {})?;
    }
    fold.terminal
        .map(|outcome| (fold.text, fold.usage, outcome))
        .ok_or(ProviderError::InvalidResponse {
            provider,
            detail: "stream ended before terminal event",
        })
}
