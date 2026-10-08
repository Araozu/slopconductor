//! Provider-neutral decoding entry points for the shared bounded wire parsers.

use serde_json::Value;
use slop_core::provider::ProviderId;

use super::{ChatResponse, ProviderError, WireProtocol, opencode};

pub(super) use opencode::{SseStream, StreamFold, is_event_stream};
pub(super) const MAX_BODY_BYTES: usize = opencode::MAX_BODY_BYTES;

pub(super) fn parse_models_list(
    provider: ProviderId,
    body: &str,
) -> Result<Vec<String>, ProviderError> {
    opencode::parse_models_list(body).map_err(|error| error.for_provider(provider))
}

pub(super) fn decode_response(
    provider: ProviderId,
    wire: WireProtocol,
    model: &str,
    value: &Value,
) -> Result<ChatResponse, ProviderError> {
    let (text, usage, outcome) = match wire {
        WireProtocol::OpenAiChatCompletions => opencode::parse_chat_response(value),
        WireProtocol::OpenAiResponses => opencode::parse_responses_response(value),
        WireProtocol::AnthropicMessages => opencode::parse_messages_response(value),
    }
    .map_err(|error| error.for_provider(provider))?;
    Ok(ChatResponse {
        model: model.to_owned(),
        resolved_model: opencode::reported_model(value)
            .map_err(|error| error.for_provider(provider))?,
        text,
        usage,
        wire,
        outcome,
    })
}
