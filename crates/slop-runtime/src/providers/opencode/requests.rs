use super::super::Role;
use super::*;

/// Translate neutral messages to OpenAI Chat `messages`.
pub(super) fn to_openai_messages(messages: &[super::super::ChatMessage]) -> Vec<Value> {
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
pub(super) fn to_responses_input(messages: &[super::super::ChatMessage]) -> Value {
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
pub(super) fn to_anthropic_parts(
    messages: &[super::super::ChatMessage],
) -> (Option<String>, Vec<Value>) {
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
