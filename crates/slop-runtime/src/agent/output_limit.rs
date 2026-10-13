use crate::providers::inference::{InferenceMessage, MessageRole};

/// Two recovery requests per accepted turn, within its existing operation budgets.
pub const MAX_CONTINUATIONS: u32 = 2;

/// Reconstructed from a durable output-limited request after pause or steering.
pub fn continuation_prompt(request_id: &str) -> InferenceMessage {
    InferenceMessage::text(
        MessageRole::User,
        format!("{request_id}:output-limit-continuation"),
        "Your previous response reached its output-token limit. Continue the original task from the visible partial response and recorded tool results, without repeating completed work. Truncated tool calls were discarded and did not execute. Keep the next response concise; split large file writes or edits into smaller operations that fit the same output limit. Do not claim an unfinished task is complete."
            .into(),
    )
}

pub fn context_continuation_prompt(request_id: &str) -> InferenceMessage {
    InferenceMessage::text(MessageRole::User, format!("{request_id}:context-limit-continuation"),
        "Your previous response reached the model's context window. After context compaction, continue the original task from the partial response and recorded results. Do not repeat completed work. Truncated tool calls were discarded and did not execute.".into())
}
