//! Transport-independent domain layer for Hive.
//!
//! Session identities, task transitions, budgets, and ownership rules belong
//! here. This crate has no networking, storage, or UI dependencies.

pub mod orchestration;
pub mod provider;
