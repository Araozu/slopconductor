use super::super::{Usage, inference::*, opencode::DecodeError};
use serde_json::{Value, json};

pub(in crate::providers) type Result<T> = std::result::Result<T, DecodeError>;

pub(in crate::providers) fn invalid(detail: &'static str) -> DecodeError {
    DecodeError::InvalidResponse { detail }
}

pub(in crate::providers) fn unsupported() -> DecodeError {
    DecodeError::UnsupportedCapability {
        capability: "unsupported structured content",
    }
}
pub(in crate::providers) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing structured string"))
}
pub(in crate::providers) fn array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value]> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| invalid("missing structured array"))
}
pub(in crate::providers) fn append(target: &mut String, text: &str, limit: usize) -> Result<()> {
    if target.len().saturating_add(text.len()) > limit {
        return Err(DecodeError::LimitExceeded {
            detail: "structured stream exceeds byte bound",
        });
    }
    target.push_str(text);
    Ok(())
}
pub(in crate::providers) fn usage(value: &Value) -> Usage {
    let u = value.get("usage").unwrap_or(&Value::Null);
    super::super::opencode::parse_usage(u)
}
pub(in crate::providers) fn role(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::Developer => "developer",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub(in crate::providers) fn tool_output(content: &BlockContent) -> String {
    match content {
        BlockContent::ToolResult{output,is_error,artifact_ids,effects_unknown,..}=>json!({"output":output,"is_error":is_error,"artifact_ids":artifact_ids,"effects_unknown":effects_unknown}).to_string(),
        _=>String::new(),
    }
}

pub(in crate::providers) fn block(
    request: &InferenceRequest,
    index: usize,
    content: BlockContent,
) -> ContentBlock {
    ContentBlock {
        id: format!("{}:block:{index}", request.request_id),
        content,
    }
}
pub(in crate::providers) fn call(
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
        || crate::json::encoded_len(&arguments).map_err(|_| invalid("invalid tool arguments"))?
            > MAX_ARGUMENT_BYTES
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
