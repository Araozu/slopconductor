//! Native agent execution layer owned by the machine daemon.
//!
//! Provider integrations, tool supervision, durable checkpoints, and bounded
//! session scheduling live here. The provider registry (`providers`) is the
//! first implemented slice: OpenCode Go over its documented wire shapes. It
//! must never depend on a client frontend.

pub mod providers;
