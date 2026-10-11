//! Headless Codex access through the public OpenAI Responses API.

mod client;
mod provider;
#[cfg(test)]
mod structured_tests;
#[cfg(test)]
mod tests;

pub use client::CodexClient;
pub use provider::{BASE_URL, CodexProvider, ENV_KEY_VAR, MODELS, model_capabilities};
