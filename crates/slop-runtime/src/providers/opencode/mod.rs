//! Shared native HTTP transport for OpenCode gateways and bounded text decoders
//! reused by all compiled-in provider clients.
//!
//! Each adapter supplies its own identity, endpoint, credential variable, and
//! verified catalog. HTTP redirects/retries are disabled, and upstream error
//! bodies never enter diagnostics. The decoder never assigns provider identity.

use std::time::Duration;

use serde_json::Value;
use slop_core::provider::{ProviderId, is_valid_model_id};

use super::{
    ChatRequest, ChatResponse, Provider, ProviderClient, ProviderError, ProviderFuture,
    StreamDelta, TurnOutcome, Usage, WireProtocol,
};

/// Required version header for the Anthropic Messages shape.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Session header used for routing and prompt caching.
pub const SESSION_HEADER: &str = "x-opencode-session";
/// This client's own identifier, shared by both OpenCode gateways.
pub const USER_AGENT: &str = concat!("slopconductor/", env!("CARGO_PKG_VERSION"));

/// Cap for blocking response bodies, enforced while reading.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Cap for one SSE line or assembled event payload.
pub const MAX_SSE_LINE_BYTES: usize = 256 * 1024;
/// Cap for assembled streamed text.
pub const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
/// Cap for total streamed bytes per request.
pub const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

mod client;
mod decode;
#[cfg(test)]
mod http_tests;
mod requests;
mod sse;
mod stream;
#[cfg(test)]
mod tests;

pub(in crate::providers) use client::{OpencodeClient, validate_base_url};
pub(in crate::providers) use decode::{
    parse_chat_response, parse_messages_response, parse_models_list, parse_responses_response,
    parse_usage, reported_model,
};
pub(in crate::providers) use sse::{DecodeError, SseStream, is_event_stream};
pub(in crate::providers) use stream::StreamFold;
