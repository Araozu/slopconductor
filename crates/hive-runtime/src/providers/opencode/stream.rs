use super::decode::{
    chat_outcome, has_content, invalid_terminal, messages_outcome, parse_responses_response,
    parse_usage, reported_model, validate_stream_content,
};
use super::sse::DecodeError;
use super::*;

/// Incremental fold of SSE lines into text, usage, and terminal outcome.
///
/// Shared by the async streaming loop and the synchronous test driver so both
/// enforce the same terminal, error, usage, and bound behavior.
#[derive(Debug, Default)]
pub(in crate::providers) struct StreamFold {
    pub(in crate::providers) text: String,
    pub(in crate::providers) resolved_model: Option<String>,
    pub(in crate::providers) usage: Usage,
    stop_reason: Option<String>,
    pub(in crate::providers) terminal: Option<TurnOutcome>,
}

impl StreamFold {
    pub(in crate::providers) fn feed(
        &mut self,
        line: &str,
        wire: WireProtocol,
        on_delta: &mut impl FnMut(StreamDelta),
    ) -> Result<(), DecodeError> {
        let payload = sse_payload(line);
        if payload.trim() == "[DONE]" {
            if wire != WireProtocol::OpenAiChatCompletions || self.terminal.is_none() {
                return Err(invalid_terminal(
                    "end marker without a valid terminal outcome",
                ));
            }
            return Ok(());
        }
        let value: Value = serde_json::from_str(&payload)
            .map_err(|_| invalid_terminal("stream event is not valid JSON"))?;
        if super::super::transport::is_context_error(&value) {
            return Err(DecodeError::ContextLengthExceeded);
        }
        if sse_stream_error(&value) {
            return Err(DecodeError::TurnFailed);
        }
        validate_stream_content(&value, wire)?;
        if let Some(model) = reported_model(&value)? {
            self.resolved_model = Some(model);
        }
        if let Some(reason) = sse_stop_reason(&value) {
            self.stop_reason = Some(reason);
        }
        if let Some(delta) = parse_stream_delta(&value, wire) {
            if self.terminal.is_some() {
                return Err(invalid_terminal("content received after terminal outcome"));
            }
            if self.text.len().saturating_add(delta.text.len()) > MAX_STREAM_TEXT_BYTES {
                return Err(DecodeError::LimitExceeded {
                    detail: "streamed text exceeds byte bound",
                });
            }
            if !delta.text.is_empty() {
                self.text.push_str(&delta.text);
            }
            on_delta(delta);
        }
        merge_sse_usage(&mut self.usage, &value);
        if let Some(outcome) = sse_terminal(&value, wire, self.stop_reason.as_deref())? {
            if self
                .terminal
                .as_ref()
                .is_some_and(|prior| *prior != outcome)
            {
                return Err(invalid_terminal("conflicting terminal outcomes"));
            }
            if wire == WireProtocol::OpenAiResponses {
                let (text, usage, _) = parse_responses_response(
                    value
                        .get("response")
                        .ok_or_else(|| invalid_terminal("terminal event has no response"))?,
                )?;
                if text.len() > MAX_STREAM_TEXT_BYTES {
                    return Err(DecodeError::LimitExceeded {
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
/// [`DecodeError::TurnFailed`] carries no upstream excerpt, so reflected
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
    value: &Value,
    wire: WireProtocol,
    stop_reason: Option<&str>,
) -> Result<Option<TurnOutcome>, DecodeError> {
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
            finish.map(|reason| chat_outcome(Some(reason))).transpose()
        }
        WireProtocol::AnthropicMessages => {
            if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                messages_outcome(stop_reason).map(Some)
            } else {
                Ok(None)
            }
        }
        WireProtocol::OpenAiResponses => match value.get("type").and_then(Value::as_str) {
            Some("response.completed" | "response.incomplete") => {
                let response = value
                    .get("response")
                    .ok_or_else(|| invalid_terminal("terminal event has no response"))?;
                let (_, _, outcome) = parse_responses_response(response)?;
                let expected =
                    if value.get("type").and_then(Value::as_str) == Some("response.completed") {
                        "completed"
                    } else {
                        "incomplete"
                    };
                if response.get("status").and_then(Value::as_str) != Some(expected) {
                    return Err(invalid_terminal(
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
    body: &str,
    wire: WireProtocol,
) -> Result<(String, Usage, TurnOutcome), DecodeError> {
    let mut stream = SseStream::new();
    let mut lines = stream.push(body.as_bytes())?;
    lines.extend(stream.finish()?);
    let mut fold = StreamFold::default();
    for line in &lines {
        fold.feed(line, wire, &mut |_| {})?;
    }
    fold.terminal
        .map(|outcome| (fold.text, fold.usage, outcome))
        .ok_or(DecodeError::InvalidResponse {
            detail: "stream ended before terminal event",
        })
}
