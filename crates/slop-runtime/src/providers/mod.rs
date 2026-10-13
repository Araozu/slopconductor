//! Native provider integrations owned by the machine daemon.
//!
//! Each provider is compiled in (no runtime plugins). All providers share the
//! same shape: a static model catalog mapping every model to one of three wire
//! protocols (OpenAI Chat Completions, OpenAI Responses, Anthropic Messages),
//! daemon-owned credentials, and provider-neutral request/response records.
//!
//! Implemented providers are OpenCode Go, OpenCode Zen, and headless Codex.

pub mod chatgpt_auth;
pub mod codex;
pub mod inference;
mod opencode;
pub mod opencode_go;
pub mod opencode_zen;
#[cfg(test)]
mod provider_tests;
mod structured_wire;
#[cfg(test)]
mod test_http;
mod transport;
mod wire;

mod wire_protocol;
pub use wire_protocol::WireProtocol;
mod context_limits;
mod provider_model;
pub use provider_model::ProviderModel;
mod provider_error;
pub use provider_error::ProviderError;
mod auth_mode;
pub use auth_mode::AuthMode;
mod provider_trait;
pub use provider_trait::Provider;
mod provider_registry;
pub use provider_registry::provider;
mod provider_client;
pub use provider_client::{ProviderClient, ProviderFuture};
mod chat;
pub use chat::{ChatMessage, ChatRequest, Role, is_valid_session_id};
mod usage;
pub use usage::{Usage, UsageSource};
mod outcomes;
pub use outcomes::{ChatResponse, StreamDelta, TurnOutcome};
use slop_core::provider::ProviderId;

pub use codex::CodexProvider;
pub use opencode_go::OpencodeGoProvider;
pub use opencode_zen::OpencodeZenProvider;
