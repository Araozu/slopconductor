//! Native agent execution layer owned by the machine daemon.
//!
//! Provider integrations, tool supervision, durable checkpoints, and bounded
//! session scheduling live here. The provider registry (`providers`) is the
//! first implemented slice: OpenCode Go, Zen, and headless Codex model access.
//! This crate must never depend on a client frontend.

pub mod agent;
pub mod chat;
#[cfg(test)]
mod chat_tests;
pub mod git;
pub mod orchestration;
pub mod providers;
pub mod tools;
