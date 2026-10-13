use std::collections::BTreeMap;

use serde_json::{Value, json};
use slop_core::provider::ProviderId;

use super::super::{Usage, WireProtocol, inference::*, opencode::DecodeError};
use super::common::{Result, append, array, invalid, string, unsupported, usage};
use super::decoding::decode;

#[derive(Default)]
struct CallParts {
    id: String,
    name: String,
    arguments: String,
}

pub(in crate::providers) struct StructuredFold {
    wire: WireProtocol,
    text: String,
    reasoning: String,
    refusal: String,
    calls: BTreeMap<usize, CallParts>,
    anthropic: BTreeMap<usize, Value>,
    argument_parts: BTreeMap<usize, String>,
    response_parts: BTreeMap<(usize, usize), (String, String)>,
    closed: std::collections::HashSet<usize>,
    terminal: Option<Value>,
    stop_reason: Option<String>,
    model: Option<String>,
    usage: Usage,
}

impl StructuredFold {
    pub(in crate::providers) fn new(wire: WireProtocol) -> Self {
        Self {
            wire,
            text: String::new(),
            reasoning: String::new(),
            refusal: String::new(),
            calls: BTreeMap::new(),
            anthropic: BTreeMap::new(),
            argument_parts: BTreeMap::new(),
            response_parts: BTreeMap::new(),
            closed: std::collections::HashSet::new(),
            terminal: None,
            stop_reason: None,
            model: None,
            usage: Usage::default(),
        }
    }

    pub(in crate::providers) fn feed(
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
        if super::super::transport::is_context_error(&value) {
            return Err(DecodeError::ContextLengthExceeded);
        }
        if value.get("error").is_some_and(|e| !e.is_null())
            || matches!(
                value.get("type").and_then(Value::as_str),
                Some("error" | "response.failed" | "response.error")
            )
        {
            return Err(DecodeError::TurnFailed);
        }
        if let Some(model) = super::super::opencode::reported_model(&value)? {
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
        append(
            &mut self.text,
            text,
            super::super::opencode::MAX_STREAM_TEXT_BYTES,
        )?;
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
                    super::super::opencode::MAX_STREAM_TEXT_BYTES,
                )?;
            }
            if let Some(text) = delta.get("reasoning_content").filter(|v| !v.is_null()) {
                if self.terminal.is_some() {
                    return Err(invalid("reasoning after terminal"));
                }
                append(
                    &mut self.reasoning,
                    text.as_str().ok_or_else(unsupported)?,
                    super::super::opencode::MAX_STREAM_TEXT_BYTES,
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
            "response.output_text.delta" | "response.refusal.delta" => {
                let content_type = if kind == "response.refusal.delta" {
                    "refusal"
                } else {
                    "output_text"
                };
                let delta = string(value, "delta")?;
                let entry = self
                    .response_parts
                    .entry((index, part))
                    .or_insert_with(|| (content_type.to_owned(), String::new()));
                if entry.0 != content_type {
                    return Err(invalid("contradictory content part"));
                }
                append(
                    &mut entry.1,
                    delta,
                    super::super::opencode::MAX_STREAM_TEXT_BYTES,
                )?;
                self.text_delta(index * MAX_BLOCKS + part, delta, on_event)
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
                // Deltas are provisional, but a completed snapshot must agree
                // with them before any proposal can reach the tool runtime.
                if expected == "completed" {
                    let output = array(response, "output")?;
                    for ((item_index, part_index), (content_type, text)) in &self.response_parts {
                        let part = output
                            .get(*item_index)
                            .and_then(|item| item.get("content"))
                            .and_then(Value::as_array)
                            .and_then(|parts| parts.get(*part_index))
                            .ok_or_else(|| invalid("streamed content missing from terminal"))?;
                        let field = if content_type == "refusal" {
                            "refusal"
                        } else {
                            "text"
                        };
                        if string(part, "type")? != content_type || string(part, field)? != text {
                            return Err(invalid("contradictory completed content"));
                        }
                    }
                    for (item_index, arguments) in &self.argument_parts {
                        let item = output
                            .get(*item_index)
                            .ok_or_else(|| invalid("streamed call missing from terminal"))?;
                        let streamed: Value = serde_json::from_str(arguments)
                            .map_err(|_| invalid("invalid completed streamed arguments"))?;
                        let completed: Value = serde_json::from_str(string(item, "arguments")?)
                            .map_err(|_| invalid("invalid completed tool arguments"))?;
                        if string(item, "type")? != "function_call" || streamed != completed {
                            return Err(invalid("contradictory completed tool arguments"));
                        }
                    }
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
                    append(
                        &mut current,
                        text,
                        super::super::opencode::MAX_STREAM_TEXT_BYTES,
                    )?;
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

    pub(in crate::providers) fn finish(
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
